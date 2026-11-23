use std::{
    collections::{BTreeMap, BTreeSet},
    mem::{size_of, size_of_val},
    num::{NonZeroU64, NonZeroUsize},
    ops::Range,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::palette::PaletteToken;
use bytemuck::{Pod, Zeroable};
use rgv_alignment_scene::{
    AlignmentCoverageLaneOptions, AlignmentModificationLaneOptions, AlignmentSashimiLaneOptions,
    AlignmentSashimiScalePolicy, AlignmentSceneSnapshot, AllocationBundleActual,
    AllocationBundleDemand, AllocationBundleError, AllocationBundleLease,
    AllocationBundleReservation, AllocationBundleSlot, AllocationClass, AllocationId,
    AllocationLease, AllocationLedger, AllocationRetirementClaimSet, AllocationRetirementToken,
    BisulfiteObservationToken, CoverageAlleleInterpretation, CpuBisulfiteStrandToken,
    CpuSummaryCamera, JunctionAnnotationStatus, LedgerError, PinnedAllocationLease,
    ReservationKind, SceneRevision, SummaryPageFamily, SummaryPageKey, SummarySemanticComponent,
    SummarySemanticPage, TransientResourceClass,
};
use rgv_core::{
    BisulfiteContext, CoverageScaleTransform, GenomicInterval, JunctionDisplayMode,
    ModifiedBaseColorMode, SashimiCountMode, SashimiLabelMode,
};
use rgv_layout::MAX_RENDERED_JUNCTION_LABELS;
use rgv_model::{
    JunctionEvidence, ModificationBin, ModificationCode, ModificationSeriesKey,
    SashimiPresentationTable,
};
use thiserror::Error;

use crate::paged_cache::{self, CachedFrameSlot, CachedGpuPage, PageTableError, PagedGpuCache};
use crate::{
    AlignmentGpuScissor, AlignmentLedgerPolicyView, GpuBudgetPressure, GpuReclaimGoal,
    GpuReclaimReport,
};

mod alleles;
mod depth;
mod depth_pyramid;
mod insertion_marks;
mod junctions;
mod modifications;

pub use alleles::*;
pub use depth::*;
pub use depth_pyramid::*;
pub use insertion_marks::*;
pub use junctions::*;
pub use modifications::*;

/// Current + next presentation. Compatible camera/scale/overlay captures alternate between these
/// physical slots and never grow the shared retained ledger during interaction.
const ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT: usize = 2;

/// Persistent device-page identity for one immutable summary page. Allele geometry names the
/// exact depth page used to join local coverage; camera, lane rectangle, scale, interaction, and
/// frame overlays are deliberately absent.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentSummaryGpuPageKey {
    semantic: SummaryPageKey,
    companion_depth: Option<SummaryPageKey>,
    /// Fingerprint of the Sashimi presentation (resolved strands, hidden junctions, splice-site
    /// totals) of this page's junctions; zero for every other page and for a junction page
    /// packed from its evidence alone.
    junction_presentation: u64,
}

impl AlignmentSummaryGpuPageKey {
    pub fn depth(page: &SummarySemanticPage) -> Result<Self, AlignmentSummaryGpuPageError> {
        if page.key().family() != SummaryPageFamily::Depth {
            return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(
                page.key().family(),
            ));
        }
        Ok(Self {
            semantic: page.key(),
            companion_depth: None,
            junction_presentation: 0,
        })
    }

    pub fn alleles(
        page: &SummarySemanticPage,
        companion_depth: &SummarySemanticPage,
    ) -> Result<Self, AlignmentSummaryGpuPageError> {
        let semantic = page.key();
        let depth = companion_depth.key();
        if semantic.family() != SummaryPageFamily::Alleles {
            return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(
                semantic.family(),
            ));
        }
        if depth.family() != SummaryPageFamily::Depth
            || depth.tile() != semantic.tile()
            || companion_depth.interval() != page.interval()
        {
            return Err(AlignmentSummaryGpuPageError::CompanionDepthMismatch {
                allele: semantic,
                depth,
            });
        }
        Ok(Self {
            semantic,
            companion_depth: Some(depth),
            junction_presentation: 0,
        })
    }

    pub fn modifications(page: &SummarySemanticPage) -> Result<Self, AlignmentSummaryGpuPageError> {
        if page.key().family() != SummaryPageFamily::Modifications {
            return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(
                page.key().family(),
            ));
        }
        Ok(Self {
            semantic: page.key(),
            companion_depth: None,
            junction_presentation: 0,
        })
    }

    pub fn junctions(page: &SummarySemanticPage) -> Result<Self, AlignmentSummaryGpuPageError> {
        if page.key().family() != SummaryPageFamily::Junctions {
            return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(
                page.key().family(),
            ));
        }
        Ok(Self {
            semantic: page.key(),
            companion_depth: None,
            junction_presentation: 0,
        })
    }

    /// A junction page drawn under a lane's Sashimi presentation. Its key changes only when the
    /// presentation of one of its own junctions changes.
    #[allow(clippy::result_large_err)]
    pub fn junctions_with_presentation(
        page: &SummarySemanticPage,
        presentation: Option<&SashimiPresentationTable>,
    ) -> Result<Self, AlignmentSummaryGpuPageError> {
        let mut key = Self::junctions(page)?;
        if let Some(presentation) = presentation {
            let start = page.interval().start();
            key.junction_presentation =
                presentation.fingerprint_for(page.primitives().iter().map(|primitive| {
                    match primitive.component {
                        SummarySemanticComponent::Junction(junction) => {
                            (junction.evidence.start, junction.evidence.end)
                        }
                        _ => (
                            start.saturating_add(primitive.start_offset),
                            start.saturating_add(primitive.end_offset),
                        ),
                    }
                }));
        }
        Ok(key)
    }

    #[must_use]
    pub const fn semantic(self) -> SummaryPageKey {
        self.semantic
    }

    #[must_use]
    pub const fn junction_presentation(self) -> u64 {
        self.junction_presentation
    }

    #[must_use]
    pub const fn companion_depth(self) -> Option<SummaryPageKey> {
        self.companion_depth
    }

    #[must_use]
    pub const fn family(self) -> SummaryPageFamily {
        self.semantic.family()
    }
}

#[derive(Clone, Copy)]
pub struct AlignmentSummaryGpuPageRequest<'a> {
    pub page: &'a Arc<SummarySemanticPage>,
    pub companion_depth: Option<&'a Arc<SummarySemanticPage>>,
    /// The lane's Sashimi presentation for a junction page.
    pub junction_presentation: Option<&'a Arc<SashimiPresentationTable>>,
}

impl<'a> AlignmentSummaryGpuPageRequest<'a> {
    #[must_use]
    pub const fn depth(page: &'a Arc<SummarySemanticPage>) -> Self {
        Self {
            page,
            companion_depth: None,
            junction_presentation: None,
        }
    }

    #[must_use]
    pub const fn alleles(
        page: &'a Arc<SummarySemanticPage>,
        companion_depth: &'a Arc<SummarySemanticPage>,
    ) -> Self {
        Self {
            page,
            companion_depth: Some(companion_depth),
            junction_presentation: None,
        }
    }

    #[must_use]
    pub const fn modifications(page: &'a Arc<SummarySemanticPage>) -> Self {
        Self {
            page,
            companion_depth: None,
            junction_presentation: None,
        }
    }

    #[must_use]
    pub const fn junctions(page: &'a Arc<SummarySemanticPage>) -> Self {
        Self {
            page,
            companion_depth: None,
            junction_presentation: None,
        }
    }

    pub fn key(self) -> Result<AlignmentSummaryGpuPageKey, AlignmentSummaryGpuPageError> {
        match self.page.key().family() {
            SummaryPageFamily::Depth if self.companion_depth.is_none() => {
                AlignmentSummaryGpuPageKey::depth(self.page)
            }
            SummaryPageFamily::Alleles => AlignmentSummaryGpuPageKey::alleles(
                self.page,
                self.companion_depth
                    .ok_or(AlignmentSummaryGpuPageError::MissingCompanionDepth(
                        self.page.key(),
                    ))?,
            ),
            SummaryPageFamily::Modifications if self.companion_depth.is_none() => {
                AlignmentSummaryGpuPageKey::modifications(self.page)
            }
            SummaryPageFamily::Junctions if self.companion_depth.is_none() => {
                AlignmentSummaryGpuPageKey::junctions_with_presentation(
                    self.page,
                    self.junction_presentation.map(Arc::as_ref),
                )
            }
            family => Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family)),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct AlignmentSummaryGpuOverlayInstance {
    origin: [f32; 2],
    size: [f32; 2],
    /// Palette index and colour parameter; see [`crate::palette::PaletteColor::encode`].
    palette_index: u32,
    color_parameter: u32,
}

impl AlignmentSummaryGpuOverlayInstance {
    fn new(origin: [f32; 2], size: [f32; 2], token: PaletteToken) -> Self {
        Self {
            origin,
            size,
            palette_index: u32::from(token.index()),
            color_parameter: 0,
        }
    }

    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32,
            3 => Uint32
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

fn projected_horizontal(
    start_offset: [f32; 2],
    end_offset: [f32; 2],
    page_start: u64,
    camera: CpuSummaryCamera,
) -> Option<(f64, f64)> {
    let clip = camera.clip();
    projected_horizontal_in_clip(
        start_offset,
        end_offset,
        page_start,
        camera,
        [clip.x, clip.x + clip.width],
    )
}

fn projected_horizontal_in_clip(
    start_offset: [f32; 2],
    end_offset: [f32; 2],
    page_start: u64,
    camera: CpuSummaryCamera,
    covered_clip_x: [f64; 2],
) -> Option<(f64, f64)> {
    projected_horizontal_exact(
        f64::from(start_offset[0]) + f64::from(start_offset[1]),
        f64::from(end_offset[0]) + f64::from(end_offset[1]),
        page_start,
        camera,
        covered_clip_x,
    )
}

/// Horizontal extent of a span given as exact page-local base offsets.
fn projected_horizontal_exact(
    start_offset: f64,
    end_offset: f64,
    page_start: u64,
    camera: CpuSummaryCamera,
    covered_clip_x: [f64; 2],
) -> Option<(f64, f64)> {
    let clip = camera.clip();
    let mut x = clip.x
        + (page_start as f64 + start_offset - camera.exact_start()) / camera.bases_per_pixel();
    let mut right =
        clip.x + (page_start as f64 + end_offset - camera.exact_start()) / camera.bases_per_pixel();
    x = x.max(clip.x).max(covered_clip_x[0]);
    right = right.min(clip.x + clip.width).min(covered_clip_x[1]);
    (right > x).then_some((x, (right - x).max(1.0)))
}

fn transformed_depth_fraction(depth: u32, maximum: u32, transform: CoverageScaleTransform) -> f64 {
    let fraction = (f64::from(depth) / f64::from(maximum.max(1))).min(1.0);
    match transform {
        CoverageScaleTransform::Linear => fraction,
        CoverageScaleTransform::SquareRoot => fraction.sqrt(),
    }
}

#[derive(Clone, Debug)]
pub enum PackedAlignmentSummaryPage {
    Depth {
        instances: Vec<AlignmentSummaryGpuDepthInstance>,
        range_maximum: AlignmentSummaryDepthRangeIndex,
    },
    Alleles {
        instances: Vec<AlignmentSummaryGpuAlleleInstance>,
    },
    Modifications {
        instances: Vec<AlignmentSummaryGpuModificationInstance>,
        metadata: AlignmentSummaryModificationPageMetadata,
    },
    Junctions {
        instances: Vec<AlignmentSummaryGpuJunctionInstance>,
        metadata: AlignmentSummaryJunctionPageMetadata,
    },
}

impl PackedAlignmentSummaryPage {
    pub fn from_request(
        request: AlignmentSummaryGpuPageRequest<'_>,
    ) -> Result<Self, AlignmentSummaryGpuPageError> {
        match request.key()?.family() {
            SummaryPageFamily::Depth => pack_depth_page(request.page),
            SummaryPageFamily::Alleles => pack_allele_page(
                request.page,
                request.companion_depth.ok_or(
                    AlignmentSummaryGpuPageError::MissingCompanionDepth(request.page.key()),
                )?,
            ),
            SummaryPageFamily::Modifications => pack_modification_page(request.page),
            SummaryPageFamily::Junctions => {
                pack_junction_page(request.page, request.junction_presentation.map(Arc::as_ref))
            }
            family => Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family)),
        }
    }

    #[must_use]
    pub fn instance_count(&self) -> usize {
        match self {
            Self::Depth { instances, .. } => instances.len(),
            Self::Alleles { instances } => instances.len(),
            Self::Modifications { instances, .. } => instances.len(),
            Self::Junctions { instances, .. } => instances.len(),
        }
    }

    #[must_use]
    pub fn uploaded_bytes(&self) -> usize {
        match self {
            Self::Depth { instances, .. } => size_of_val(instances.as_slice()),
            Self::Alleles { instances } => size_of_val(instances.as_slice()),
            Self::Modifications { instances, .. } => size_of_val(instances.as_slice()),
            Self::Junctions { instances, .. } => size_of_val(instances.as_slice()),
        }
    }

    fn range_index_bytes(&self) -> usize {
        match self {
            // The index value itself lives inline in `GpuSummaryPageMetadata` and is charged by
            // the page-table allocation. Only its owned vectors belong to the per-page lease.
            Self::Depth { range_maximum, .. } => range_maximum.owned_heap_bytes(),
            Self::Alleles { .. } => 0,
            Self::Modifications { metadata, .. } => metadata.owned_heap_bytes(),
            Self::Junctions { metadata, .. } => metadata.owned_heap_bytes(),
        }
    }
}

fn safe_fraction(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AlignmentSummaryFrameOverlays {
    pub selected_position: Option<u64>,
    pub hovered_junction: Option<AlignmentSummaryJunctionInteraction>,
    pub selected_junction: Option<AlignmentSummaryJunctionInteraction>,
}

/// Stable semantic identity used to restyle one persistent Sashimi component without rebuilding
/// or uploading its geometry. Coordinates are absolute half-open reference positions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSummaryJunctionInteraction {
    pub start: u64,
    pub end: u64,
    pub component: AlignmentSummaryJunctionComponentToken,
}

/// Typed renderer-facing lane contract. Camera-independent semantic page identity is selected by
/// the variant; live filtering, weighting, colouring, and marker state stay in the frame request.
#[derive(Clone, Debug, PartialEq)]
pub enum AlignmentSummaryGpuLaneOptions {
    Coverage(AlignmentCoverageLaneOptions),
    Sashimi(AlignmentSashimiLaneOptions),
    Modifications(AlignmentModificationLaneOptions),
}

impl From<AlignmentCoverageLaneOptions> for AlignmentSummaryGpuLaneOptions {
    fn from(options: AlignmentCoverageLaneOptions) -> Self {
        Self::Coverage(options)
    }
}

impl From<AlignmentModificationLaneOptions> for AlignmentSummaryGpuLaneOptions {
    fn from(options: AlignmentModificationLaneOptions) -> Self {
        Self::Modifications(options)
    }
}

impl From<AlignmentSashimiLaneOptions> for AlignmentSummaryGpuLaneOptions {
    fn from(options: AlignmentSashimiLaneOptions) -> Self {
        Self::Sashimi(options)
    }
}

pub struct AlignmentSummaryGpuSceneRequest<'a> {
    pub scene: &'a Arc<AlignmentSceneSnapshot>,
    /// Panel-local coherence namespace. Shared scales never cross this stable boundary.
    pub coherence_scope: u64,
    /// Stable lane identity within the coherence scope, carried into scene-native interaction.
    pub lane_identity: u64,
    pub camera_sequence: u64,
    pub camera: CpuSummaryCamera,
    pub screen_size_points: [f32; 2],
    pub scissor: AlignmentGpuScissor,
    pub options: AlignmentSummaryGpuLaneOptions,
    pub overlays: AlignmentSummaryFrameOverlays,
    /// Resolved strands, hidden junctions and splice-site totals for a Sashimi lane. `None`
    /// draws every junction from its own evidence (strand tags only, nothing hidden).
    pub junction_presentation: Option<Arc<SashimiPresentationTable>>,
}

fn summary_content_selectors(
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
) -> Vec<AlignmentSummaryGpuContentSelector> {
    requests
        .iter()
        .map(|request| match &request.options {
            AlignmentSummaryGpuLaneOptions::Coverage(options) => {
                AlignmentSummaryGpuContentSelector::Coverage {
                    alleles: options.alleles.is_some(),
                }
            }
            AlignmentSummaryGpuLaneOptions::Modifications(_) => {
                AlignmentSummaryGpuContentSelector::Modifications
            }
            AlignmentSummaryGpuLaneOptions::Sashimi(_) => {
                AlignmentSummaryGpuContentSelector::Junctions {
                    presentation: request
                        .junction_presentation
                        .as_ref()
                        .map_or(0, |table| table.fingerprint()),
                }
            }
        })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct AlignmentSummaryGpuUniform {
    screen_size: [f32; 2],
    clip_origin: [f32; 2],
    clip_size: [f32; 2],
    points_per_base: [f32; 2],
    /// Whole bases from the camera start to the page start, rounded down.
    page_start_whole: i32,
    /// The remainder, in `[0, 1)`.
    page_start_fraction: f32,
    covered_clip_x: [f32; 2],
    scale_maximum: f32,
    minimum_alternate_fraction: f32,
    transform: u32,
    quality_weighted: u32,
    family: u32,
    hidden_modification_mask: u32,
    modification_color_mode: u32,
    junction_normalization: f32,
    junction_row_count: u32,
    minimum_junction_depth: u32,
    minimum_junction_flank: u32,
    junction_display_mode: u32,
    allele_interpretation: u32,
    bisulfite_context_mask: u32,
    /// Bit 0: junction labels reserve room above each apex.
    junction_flags: u32,
    padding: u32,
    hovered_junction_span: [f32; 4],
    selected_junction_span: [f32; 4],
    junction_interaction: [u32; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSummaryGpuPrepareStats {
    pub scenes: usize,
    pub mandatory_pages: usize,
    pub depth_instances: usize,
    pub allele_instances: usize,
    pub junction_instances: usize,
    pub modification_instances: usize,
    pub overlay_instances: usize,
    pub camera_uniform_bytes: usize,
    pub geometry_uploaded_bytes: usize,
    pub range_maximum_queries: usize,
    pub junction_entries_examined: usize,
    pub page_pin_acquisitions: usize,
    pub page_cache_hits: usize,
    pub page_cache_misses: usize,
    pub resident_pages: usize,
    pub resident_gpu_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentSummaryGpuPageAdmission {
    Admitted {
        instances: usize,
        uploaded_bytes: usize,
    },
    AlreadyResident,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSummaryGpuBatchAdmission {
    pub requested_pages: usize,
    pub admitted_pages: usize,
    pub already_resident_pages: usize,
    pub instances: usize,
    pub uploaded_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSummaryGpuPrefetchProgress {
    pub prepared_pages: usize,
    pub missing_pages_before: usize,
    pub admission: AlignmentSummaryGpuBatchAdmission,
    pub remaining_missing_pages: usize,
    pub missing_pages_remain: bool,
    pub retained_bytes: usize,
    pub retention_limited: bool,
}

/// Resumable exact-summary warm-up bound to one immutable prepared-page authority. Semantic pages
/// are resolved lazily from retained scene identities, so cursor creation is O(lanes) and every
/// UI-frame step remains bounded. Existing and newly admitted pages stay pinned while it advances.
#[must_use]
pub struct AlignmentSummaryGpuPrefetchCursor {
    device_generation: u64,
    page_keys: Arc<[AlignmentSummaryGpuPageKey]>,
    scenes: Vec<(
        Arc<AlignmentSceneSnapshot>,
        AlignmentSummaryGpuContentSelector,
        Option<Arc<SashimiPresentationTable>>,
    )>,
    next_page: usize,
    pins: Vec<PinnedAllocationLease>,
    retained_bytes: usize,
    maximum_total_pinned_bytes: usize,
    retention_limited: bool,
}

impl AlignmentSummaryGpuPrefetchCursor {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.next_page == self.page_keys.len()
    }

    #[must_use]
    pub fn remaining_pages(&self) -> usize {
        self.page_keys.len().saturating_sub(self.next_page)
    }

    #[must_use]
    pub const fn retention_limited(&self) -> bool {
        self.retention_limited
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

/// Constant-size renderer instrumentation proving that compatible camera/options capture does
/// not regenerate the complete prepared summary-page manifest.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSummaryGpuRendererOperationCounters {
    pub content_manifest_builds: u64,
    pub content_manifest_pages_enumerated: u64,
    pub prefetch_authority_keys_inspected: u64,
    pub reclaim_authority_keys_inspected: u64,
}

impl AlignmentSummaryGpuPrefetchProgress {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        !self.missing_pages_remain
    }
}

#[derive(Debug)]
#[must_use = "dropping the residency lease immediately makes its pages evictable"]
pub struct AlignmentSummaryGpuResidencyLease {
    device_generation: u64,
    page_keys: Arc<[AlignmentSummaryGpuPageKey]>,
    _pins: Vec<PinnedAllocationLease>,
}

impl AlignmentSummaryGpuResidencyLease {
    #[must_use]
    pub const fn device_generation(&self) -> u64 {
        self.device_generation
    }

    #[must_use]
    pub fn page_keys(&self) -> &[AlignmentSummaryGpuPageKey] {
        &self.page_keys
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.page_keys.len()
    }
}

struct PendingSummaryPageAdmission {
    key: AlignmentSummaryGpuPageKey,
    instance_count: u32,
    buffer_capacity: usize,
    retained_capacity: usize,
    packed: PackedAlignmentSummaryPage,
}

#[derive(Debug)]
enum GpuSummaryPageMetadata {
    Depth(AlignmentSummaryDepthRangeIndex),
    Alleles,
    Junctions(AlignmentSummaryJunctionPageMetadata),
    Modifications(AlignmentSummaryModificationPageMetadata),
}

#[derive(Debug)]
struct GpuSummaryPage {
    key: AlignmentSummaryGpuPageKey,
    interval_start: u64,
    buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    metadata: GpuSummaryPageMetadata,
    allocation: Option<AllocationLease>,
    last_used_revision: u64,
}

impl CachedGpuPage for GpuSummaryPage {
    type Key = AlignmentSummaryGpuPageKey;

    fn key(&self) -> AlignmentSummaryGpuPageKey {
        self.key
    }

    fn last_used_revision(&self) -> u64 {
        self.last_used_revision
    }

    fn set_last_used_revision(&mut self, revision: u64) {
        self.last_used_revision = revision;
    }

    fn allocation(&self) -> Option<&AllocationLease> {
        self.allocation.as_ref()
    }
}

#[derive(Clone, Debug)]
struct PreparedSummaryDraw {
    page: AlignmentSummaryGpuPageKey,
    coherence_scope: u64,
    lane_identity: u64,
    camera: CpuSummaryCamera,
    page_start: u64,
    junction_scale_maximum: f32,
    junction_normalization: f32,
    junction_row_count: u32,
    minimum_junction_depth: u32,
    minimum_junction_flank: u32,
    junction_display_mode: JunctionDisplayMode,
    junction_count_mode: SashimiCountMode,
    junction_label_mode: SashimiLabelMode,
    show_junction_labels: bool,
    uniform_offset: u32,
    scissor: AlignmentGpuScissor,
    background_range: Range<u32>,
    foreground_range: Range<u32>,
}

#[derive(Clone, Debug)]
struct PendingSummaryDraw {
    page: AlignmentSummaryGpuPageKey,
    coherence_scope: u64,
    lane_identity: u64,
    camera: CpuSummaryCamera,
    screen_size: [f32; 2],
    scissor: AlignmentGpuScissor,
    page_start: u64,
    covered_clip_x: [f32; 2],
    scale_maximum: u32,
    minimum_alternate_fraction: f32,
    transform: CoverageScaleTransform,
    quality_weighted: bool,
    allele_interpretation: CoverageAlleleInterpretation,
    hidden_modification_mask: u32,
    modification_color_mode: ModifiedBaseColorMode,
    junction_scale_maximum: f32,
    junction_normalization: f32,
    junction_row_count: u32,
    minimum_junction_depth: u32,
    minimum_junction_flank: u32,
    junction_display_mode: JunctionDisplayMode,
    junction_count_mode: SashimiCountMode,
    junction_label_mode: SashimiLabelMode,
    show_junction_labels: bool,
    overlays: AlignmentSummaryFrameOverlays,
    background_range: Range<u32>,
    foreground_range: Range<u32>,
}

#[derive(Debug)]
struct AlignmentSummaryGpuResidentPageLease {
    key: AlignmentSummaryGpuPageKey,
    allocation: AllocationId,
    pin: PinnedAllocationLease,
}

#[derive(Debug, Default)]
struct AlignmentSummaryGpuContentLeaseCache {
    pages: Mutex<BTreeMap<AlignmentSummaryGpuPageKey, Weak<AlignmentSummaryGpuResidentPageLease>>>,
}

impl AlignmentSummaryGpuContentLeaseCache {
    fn acquire(
        &self,
        key: AlignmentSummaryGpuPageKey,
        allocation: &AllocationLease,
    ) -> Result<(Arc<AlignmentSummaryGpuResidentPageLease>, bool), LedgerError> {
        let mut pages = self.pages.lock().map_err(|_| LedgerError::Poisoned)?;
        if let Some(existing) = pages.get(&key).and_then(Weak::upgrade)
            && existing.allocation == allocation.id()
        {
            return Ok((existing, false));
        }
        let lease = Arc::new(AlignmentSummaryGpuResidentPageLease {
            key,
            allocation: allocation.id(),
            pin: allocation.pin()?,
        });
        pages.insert(key, Arc::downgrade(&lease));
        Ok((lease, true))
    }
}

#[derive(Clone, Debug)]
struct SummaryDrawStyle {
    minimum_alternate_fraction: f32,
    transform: CoverageScaleTransform,
    quality_weighted: bool,
    allele_interpretation: CoverageAlleleInterpretation,
    hidden_modification_mask: u32,
    modification_color_mode: ModifiedBaseColorMode,
    junction_normalization: f32,
    junction_row_count: u32,
    minimum_junction_depth: u32,
    minimum_junction_flank: u32,
    junction_display_mode: JunctionDisplayMode,
    background_range: Range<u32>,
    foreground_range: Range<u32>,
}

#[derive(Clone, Debug)]
struct PreparedSummaryOverlayDraw {
    uniform_offset: u32,
    scissor: AlignmentGpuScissor,
    range: Range<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlignmentSummaryGpuContentSelector {
    Coverage { alleles: bool },
    Modifications,
    Junctions { presentation: u64 },
}

/// Camera-independent content authority for an exact-summary presentation. Replacement
/// publication mints the ordered immutable scene identities, lane content selectors, and complete
/// prepared-page manifest once. Cameras, scale transforms, thresholds, hidden modification codes,
/// colours, and overlays stay outside this token and may therefore change through the no-bundle
/// capture path.
///
/// Camera capture validates only the constant-size-per-lane identities/selectors. The manifest
/// remains exact replacement/retirement authority but is not rebuilt during navigation.
#[derive(Clone, Debug)]
pub struct AlignmentSummaryGpuContentAuthority {
    device_generation: u64,
    scene_revisions: Arc<[SceneRevision]>,
    scene_allocations: Arc<[AllocationId]>,
    content_selectors: Arc<[AlignmentSummaryGpuContentSelector]>,
    page_manifest: Arc<[AlignmentSummaryGpuPageKey]>,
    leases: Arc<AlignmentSummaryGpuContentLeaseCache>,
}

impl AlignmentSummaryGpuContentAuthority {
    fn new(
        device_generation: u64,
        scene_revisions: Arc<[SceneRevision]>,
        scene_allocations: Arc<[AllocationId]>,
        content_selectors: Arc<[AlignmentSummaryGpuContentSelector]>,
        page_manifest: Arc<[AlignmentSummaryGpuPageKey]>,
    ) -> Self {
        Self {
            device_generation,
            scene_revisions,
            scene_allocations,
            content_selectors,
            page_manifest,
            leases: Arc::new(AlignmentSummaryGpuContentLeaseCache::default()),
        }
    }

    #[must_use]
    pub const fn device_generation(&self) -> u64 {
        self.device_generation
    }

    #[must_use]
    pub fn scene_revisions(&self) -> &[SceneRevision] {
        &self.scene_revisions
    }

    #[must_use]
    pub fn page_manifest(&self) -> &[AlignmentSummaryGpuPageKey] {
        &self.page_manifest
    }
}

impl PartialEq for AlignmentSummaryGpuContentAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.device_generation == other.device_generation
            && self.scene_revisions == other.scene_revisions
            && self.scene_allocations == other.scene_allocations
            && self.content_selectors == other.content_selectors
            && self.page_manifest == other.page_manifest
    }
}

impl Eq for AlignmentSummaryGpuContentAuthority {}

#[derive(Debug)]
pub struct AlignmentSummaryGpuFrame {
    revision: u64,
    device_generation: u64,
    pub scene_revisions: Arc<[SceneRevision]>,
    pub camera_sequences: Arc<[u64]>,
    pub resolved_depth_scales: Arc<[u32]>,
    content_authority: AlignmentSummaryGpuContentAuthority,
    draws: Arc<[PreparedSummaryDraw]>,
    overlay_draws: Arc<[PreparedSummaryOverlayDraw]>,
    buffers: Arc<GpuSummaryFrameBuffers>,
    _page_leases: Vec<Arc<AlignmentSummaryGpuResidentPageLease>>,
    _pins: Vec<PinnedAllocationLease>,
}

impl AlignmentSummaryGpuFrame {
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }

    #[must_use]
    pub fn overlay_draw_count(&self) -> usize {
        self.overlay_draws.len()
    }

    /// Whether this exact frame carries a scene-native junction lane for the requested panel and
    /// track identities. Callers use this to distinguish an authoritative miss from a lane that
    /// has not yet migrated to the native presenter.
    #[must_use]
    pub fn has_junction_lane(&self, coherence_scope: u64, lane_identity: u64) -> bool {
        self.draws.iter().any(|draw| {
            draw.page.family() == SummaryPageFamily::Junctions
                && draw.coherence_scope == coherence_scope
                && draw.lane_identity == lane_identity
        })
    }

    /// Camera-independent authority which remains valid across pan, zoom, scale, filtering, and
    /// marker-only presentation captures.
    #[must_use]
    pub const fn content_authority(&self) -> &AlignmentSummaryGpuContentAuthority {
        &self.content_authority
    }
}

#[derive(Debug)]
struct GpuSummaryFrameBuffers {
    uniform_buffer: Option<wgpu::Buffer>,
    uniform_bind_group: Option<wgpu::BindGroup>,
    uniform_allocation: Option<AllocationLease>,
    uniform_capacity: usize,
    overlay_buffer: Option<wgpu::Buffer>,
    overlay_allocation: Option<AllocationLease>,
    overlay_capacity: usize,
    frame_allocation: AllocationLease,
    frame_capacity: usize,
    capture_ring: bool,
}

impl CachedFrameSlot for GpuSummaryFrameBuffers {
    fn frame_allocation(&self) -> &AllocationLease {
        &self.frame_allocation
    }

    fn capture_ring(&self) -> bool {
        self.capture_ring
    }

    fn gpu_allocations(&self) -> [Option<&AllocationLease>; 2] {
        [
            self.uniform_allocation.as_ref(),
            self.overlay_allocation.as_ref(),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct PlannedSummaryFrameSlot {
    uniform_slot: Option<AllocationBundleSlot>,
    overlay_slot: Option<AllocationBundleSlot>,
    frame_slot: AllocationBundleSlot,
}

struct PlannedSummaryPage {
    request: OwnedSummaryPageRequest,
    slot: AllocationBundleSlot,
    maximum_bytes: usize,
}

/// Retained-capacity inventory for one complete multi-lane summary replacement. Planning creates
/// no GPU objects, performs no page packing, and touches neither the renderer cache nor the
/// ledger. Its deterministic slots can therefore be concatenated with the read renderer's plan
/// into one shared allocation bundle.
#[must_use]
pub struct AlignmentSummaryGpuReplacementPlan {
    policy_revision: u64,
    first_bundle_slot: AllocationBundleSlot,
    next_bundle_slot: AllocationBundleSlot,
    renderer_revision: u64,
    device_generation: u64,
    scene_revisions: Arc<[SceneRevision]>,
    scene_allocations: Arc<[AllocationId]>,
    visible_pages: Vec<Vec<OwnedSummaryPageRequest>>,
    mandatory_keys: Vec<AlignmentSummaryGpuPageKey>,
    content_selectors: Vec<AlignmentSummaryGpuContentSelector>,
    prepared_page_keys: Arc<[AlignmentSummaryGpuPageKey]>,
    missing_pages: Vec<PlannedSummaryPage>,
    demands: Vec<AllocationBundleDemand>,
    page_table_slot: Option<AllocationBundleSlot>,
    page_table_requested_capacity: usize,
    page_table_maximum_bytes: usize,
    uniform_capacity: usize,
    uniform_bytes: usize,
    overlay_capacity: usize,
    overlay_bytes: usize,
    frame_slots: [PlannedSummaryFrameSlot; ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT],
    maximum_frame_bytes: usize,
    request_count: usize,
    mandatory_draws: usize,
    overlay_draws: usize,
    overlay_instances: usize,
    page_cache_hits: usize,
    page_cache_misses: usize,
    transient_bytes: usize,
}

impl AlignmentSummaryGpuReplacementPlan {
    #[must_use]
    pub const fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    #[must_use]
    pub fn demands(&self) -> &[AllocationBundleDemand] {
        &self.demands
    }

    #[must_use]
    pub const fn first_bundle_slot(&self) -> AllocationBundleSlot {
        self.first_bundle_slot
    }

    #[must_use]
    pub const fn next_bundle_slot(&self) -> AllocationBundleSlot {
        self.next_bundle_slot
    }

    #[must_use]
    pub fn prepared_page_keys(&self) -> &Arc<[AlignmentSummaryGpuPageKey]> {
        &self.prepared_page_keys
    }
}

struct RetiredSummaryFrameBuffer {
    index: usize,
    buffers: Arc<GpuSummaryFrameBuffers>,
}

/// Exact prior summary presentation detached before the coordinator reserves the replacement.
/// It is deliberately non-Clone: the ledger-minted claim remains valid only while this token
/// physically owns every lease and pin named by that claim.
#[must_use = "restore this retirement after refusal or drop it before bundle settlement"]
pub struct AlignmentSummaryGpuRetirement {
    claim_set: AllocationRetirementClaimSet,
    prior_frames: Vec<AlignmentSummaryGpuFrame>,
    retired_pages: Vec<GpuSummaryPage>,
    retired_page_table: Option<(AllocationLease, usize)>,
    retired_frame_buffers: Vec<RetiredSummaryFrameBuffer>,
    renderer_revision: u64,
    device_generation: u64,
}

impl AllocationRetirementToken for AlignmentSummaryGpuRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

impl AlignmentSummaryGpuRetirement {
    #[must_use]
    pub fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

struct StagedSummaryPage {
    key: AlignmentSummaryGpuPageKey,
    interval_start: u64,
    slot: AllocationBundleSlot,
    buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    metadata: GpuSummaryPageMetadata,
    charged_capacity: usize,
}

struct StagedSummaryFrameBuffers {
    uniform_buffer: Option<wgpu::Buffer>,
    uniform_bind_group: Option<wgpu::BindGroup>,
    uniform_slot: Option<AllocationBundleSlot>,
    uniform_capacity: usize,
    overlay_buffer: Option<wgpu::Buffer>,
    overlay_slot: Option<AllocationBundleSlot>,
    overlay_capacity: usize,
    frame_slot: AllocationBundleSlot,
}

/// Fully built multi-lane summary candidate. GPU objects remain private and uncharged until the
/// shared bundle commits; publication is possible only after old retirement and settlement.
#[must_use = "a staged summary replacement must be published or dropped"]
pub struct AlignmentSummaryGpuStagedReplacement {
    plan: AlignmentSummaryGpuReplacementPlan,
    pages: Vec<StagedSummaryPage>,
    page_table: Option<Vec<GpuSummaryPage>>,
    buffers: Vec<StagedSummaryFrameBuffers>,
    scene_revisions: Vec<SceneRevision>,
    scene_allocations: Vec<AllocationId>,
    camera_sequences: Vec<u64>,
    resolved_depth_scales: Vec<u32>,
    draws: Vec<PreparedSummaryDraw>,
    overlay_draws: Vec<PreparedSummaryOverlayDraw>,
    existing_pins: Vec<PinnedAllocationLease>,
    actuals: Vec<AllocationBundleActual>,
    admission: AlignmentSummaryGpuBatchAdmission,
    depth_instances: usize,
    allele_instances: usize,
    junction_instances: usize,
    modification_instances: usize,
    camera_uniform_bytes: usize,
    geometry_uploaded_bytes: usize,
    range_maximum_queries: usize,
    junction_entries_examined: usize,
}

impl AlignmentSummaryGpuStagedReplacement {
    #[must_use]
    pub fn actuals(&self) -> &[AllocationBundleActual] {
        &self.actuals
    }
}

/// A staged summary candidate whose renderer/cache invariants have been checked before bundle
/// commit. The coordinator can preflight both read and summary candidates before it irreversibly
/// retires either old presentation.
#[must_use = "bind the settled bundle leases before publication"]
pub struct AlignmentSummaryGpuReadyReplacement {
    staged: AlignmentSummaryGpuStagedReplacement,
}

impl AlignmentSummaryGpuReadyReplacement {
    #[must_use]
    pub fn actuals(&self) -> &[AllocationBundleActual] {
        self.staged.actuals()
    }
}

/// A fully validated summary candidate which owns all of its settled allocation leases. Applying
/// it to an unchanged renderer is an infallible move-only publication step.
#[must_use = "publish this prepared summary replacement"]
pub struct AlignmentSummaryGpuPublishableReplacement {
    staged: AlignmentSummaryGpuStagedReplacement,
    leases: Vec<AllocationBundleLease>,
}

/// Persistent depth/allele renderer for immutable exact summary pages. Base buffers are uploaded
/// only on page admission; live cameras, compatible zoom, scale, and overlays occupy a small
/// transition-frame ring.
pub struct AlignmentSummaryGpuRenderer {
    depth_pipeline: wgpu::RenderPipeline,
    allele_pipeline: wgpu::RenderPipeline,
    junction_pipeline: wgpu::RenderPipeline,
    modification_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_stride: usize,
    /// Semantic palette uniform (bind group 1); see `AlignmentGpuRenderer`.
    palette: crate::palette::PaletteBinding,
    frame_buffers: Vec<Arc<GpuSummaryFrameBuffers>>,
    /// Sorted device-page table, its ledger charge and the incrementally maintained LRU
    /// authority used to produce bounded pressure-time reclaim results.
    cache: PagedGpuCache<GpuSummaryPage>,
    revision: u64,
    device_generation: u64,
    pending_geometry_uploaded_bytes: usize,
    content_manifest_builds: AtomicU64,
    content_manifest_pages_enumerated: AtomicU64,
    prefetch_authority_keys_inspected: AtomicU64,
    reclaim_authority_keys_inspected: AtomicU64,
}

impl AlignmentSummaryGpuRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self::with_palette(
            device,
            target_format,
            &crate::palette::AlignmentPalette::light(),
        )
    }

    /// The palette summary colours are currently resolved against.
    #[must_use]
    pub const fn palette(&self) -> &crate::palette::AlignmentPalette {
        self.palette.palette()
    }

    /// Recolours every resident and future summary page by writing the 2 KiB palette uniform;
    /// no page buffer is re-uploaded. Returns whether the palette changed.
    pub fn set_palette(
        &mut self,
        queue: &wgpu::Queue,
        palette: &crate::palette::AlignmentPalette,
    ) -> bool {
        self.palette.set(queue, palette)
    }

    /// Number of palette uniform uploads since this renderer was created.
    #[must_use]
    pub const fn palette_uploads(&self) -> u64 {
        self.palette.uploads()
    }

    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn with_palette(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        palette: &crate::palette::AlignmentPalette,
    ) -> Self {
        let shader = crate::shaders::ALIGNMENT_SUMMARY.create_module(device);
        let palette_layout = crate::palette::PaletteBinding::layout(device);
        let palette = crate::palette::PaletteBinding::new(device, &palette_layout, palette);
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV alignment summary uniform layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(
                        size_of::<AlignmentSummaryGpuUniform>() as u64
                    ),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV alignment summary pipeline layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&palette_layout)],
            immediate_size: 0,
        });
        let target = || {
            Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = |label, entry_point, buffers: &[wgpu::VertexBufferLayout<'static>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[target()],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let depth_layout = AlignmentSummaryGpuDepthInstance::vertex_buffer_layout();
        let allele_layout = AlignmentSummaryGpuAlleleInstance::vertex_buffer_layout();
        let modification_layout = AlignmentSummaryGpuModificationInstance::vertex_buffer_layout();
        let junction_layout = AlignmentSummaryGpuJunctionInstance::vertex_buffer_layout();
        let overlay_layout = AlignmentSummaryGpuOverlayInstance::vertex_buffer_layout();
        Self {
            depth_pipeline: pipeline(
                "RGV alignment depth pipeline",
                "depth_vertex",
                &[depth_layout],
            ),
            allele_pipeline: pipeline(
                "RGV alignment allele pipeline",
                "allele_vertex",
                &[allele_layout],
            ),
            junction_pipeline: pipeline(
                "RGV alignment junction pipeline",
                "junction_vertex",
                &[junction_layout],
            ),
            modification_pipeline: pipeline(
                "RGV alignment modification pipeline",
                "modification_vertex",
                &[modification_layout],
            ),
            overlay_pipeline: pipeline(
                "RGV alignment summary overlay pipeline",
                "overlay_vertex",
                &[overlay_layout],
            ),
            uniform_layout,
            uniform_stride: aligned_uniform_stride(device),
            palette,
            frame_buffers: Vec::new(),
            cache: PagedGpuCache::default(),
            revision: 0,
            device_generation: 1,
            pending_geometry_uploaded_bytes: 0,
            content_manifest_builds: AtomicU64::new(0),
            content_manifest_pages_enumerated: AtomicU64::new(0),
            prefetch_authority_keys_inspected: AtomicU64::new(0),
            reclaim_authority_keys_inspected: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn operation_counters(&self) -> AlignmentSummaryGpuRendererOperationCounters {
        AlignmentSummaryGpuRendererOperationCounters {
            content_manifest_builds: self.content_manifest_builds.load(Ordering::Relaxed),
            content_manifest_pages_enumerated: self
                .content_manifest_pages_enumerated
                .load(Ordering::Relaxed),
            prefetch_authority_keys_inspected: self
                .prefetch_authority_keys_inspected
                .load(Ordering::Relaxed),
            reclaim_authority_keys_inspected: self
                .reclaim_authority_keys_inspected
                .load(Ordering::Relaxed),
        }
    }

    fn record_content_manifest_build(&self, pages: usize) {
        self.content_manifest_builds.fetch_add(1, Ordering::Relaxed);
        self.content_manifest_pages_enumerated
            .fetch_add(pages as u64, Ordering::Relaxed);
    }

    /// Inventories every retained object required to replace all supplied summary lanes as one
    /// frame. No semantic page is packed here: the bounds are derived directly from immutable
    /// primitive counts so a shared coordinator can reserve the read and summary candidates
    /// together before either renderer performs expensive work.
    pub fn plan_replacement(
        &self,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &impl AlignmentLedgerPolicyView,
        first_bundle_slot: AllocationBundleSlot,
    ) -> Result<AlignmentSummaryGpuReplacementPlan, AlignmentSummaryGpuReplacementError> {
        for request in requests {
            validate_summary_request(request)?;
        }
        let visible_pages = requests
            .iter()
            .map(|request| summary_requests_for_scene(request, true))
            .collect::<Result<Vec<_>, _>>()?;
        let content_selectors = summary_content_selectors(requests);
        let prepared_page_keys: Arc<[AlignmentSummaryGpuPageKey]> =
            unique_summary_requests(requests, false)?
                .into_iter()
                .map(|request| request.key)
                .collect::<Vec<_>>()
                .into();
        self.record_content_manifest_build(prepared_page_keys.len());
        let scene_revisions = requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>()
            .into();
        let scene_allocations = requests
            .iter()
            .map(|request| request.scene.metadata_allocation().id())
            .collect::<Vec<_>>()
            .into();
        let mandatory_draws = visible_pages.iter().map(Vec::len).sum::<usize>();
        let overlay_instances = requests.iter().map(summary_overlay_count).sum::<usize>();
        u32::try_from(overlay_instances)
            .map_err(|_| AlignmentSummaryGpuPrepareError::TooManyOverlayPrimitives)?;
        let overlay_draws = requests
            .iter()
            .filter(|request| summary_overlay_count(request) > 0)
            .count();
        let uniform_count = mandatory_draws
            .checked_add(overlay_draws)
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        if uniform_count > 0 {
            u32::try_from(
                (uniform_count - 1)
                    .checked_mul(self.uniform_stride)
                    .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?,
            )
            .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?;
        }

        let mut unique = BTreeMap::new();
        let mut page_cache_hits = 0_usize;
        let mut page_cache_misses = 0_usize;
        for pages in &visible_pages {
            for page in pages {
                unique.entry(page.key).or_insert_with(|| page.clone());
                if self.page_index(page.key).is_some() {
                    page_cache_hits = page_cache_hits.saturating_add(1);
                } else {
                    page_cache_misses = page_cache_misses.saturating_add(1);
                }
            }
        }
        let mandatory_keys = unique.keys().copied().collect::<Vec<_>>();
        let mut next_slot = first_bundle_slot.get();
        let mut demands = Vec::new();
        let mut missing_pages = Vec::new();
        for request in unique
            .into_values()
            .filter(|request| self.page_index(request.key).is_none())
        {
            let maximum_bytes = summary_page_maximum_retained_bytes(&request)?;
            let slot = take_summary_bundle_slot(&mut next_slot)?;
            demands.push(AllocationBundleDemand::new(
                slot,
                AllocationClass::GpuPage,
                maximum_bytes,
            ));
            missing_pages.push(PlannedSummaryPage {
                request,
                slot,
                maximum_bytes,
            });
        }

        let required_page_capacity = self
            .cache
            .pages
            .len()
            .checked_add(missing_pages.len())
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        let (page_table_slot, page_table_requested_capacity, page_table_maximum_bytes) =
            if required_page_capacity > self.cache.page_table_capacity {
                let requested = next_summary_capacity(required_page_capacity)?;
                let maximum = requested
                    .checked_mul(2)
                    .and_then(summary_page_table_retained_bytes)
                    .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
                let slot = take_summary_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    maximum,
                ));
                (Some(slot), requested, maximum)
            } else {
                (None, 0, 0)
            };

        // Size physical slots for the maximum canonical tiles one compatible camera can expose,
        // not every summary page retained by the content authority. Complete exact summaries
        // remain resident/pageable independently of per-frame uniforms.
        let semantic_draw_envelope = requests.iter().try_fold(0_usize, |total, request| {
            let families = match &request.options {
                AlignmentSummaryGpuLaneOptions::Coverage(options) => {
                    1_usize.saturating_add(usize::from(options.alleles.is_some()))
                }
                AlignmentSummaryGpuLaneOptions::Modifications(_) => 1,
                AlignmentSummaryGpuLaneOptions::Sashimi(_) => 1,
            };
            // The immutable content authority covers the complete prepared genomic horizon.
            // Precharge its maximum horizontal camera envelope so zoom-out remains a camera-only
            // capture even while an unrelated replacement bundle is active.
            let tiles = request
                .scene
                .summary_tile_envelope(request.scene.prepared_interval().len());
            total
                .checked_add(tiles.saturating_mul(families))
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
        })?;
        let overlay_draw_envelope = requests.len();
        // Centre line and selected position, plus the insertion marks a camera-only zoom could
        // reveal anywhere in the prepared horizon of a coverage lane with allele detail.
        let overlay_instance_envelope = requests.iter().try_fold(0_usize, |total, request| {
            let insertion_marks = match &request.options {
                AlignmentSummaryGpuLaneOptions::Coverage(options) if options.alleles.is_some() => {
                    usize::try_from(request.scene.prepared_interval().len())
                        .unwrap_or(usize::MAX)
                        .min(MAXIMUM_INSERTION_MARKS_PER_LANE)
                        .saturating_mul(INSERTION_MARK_RECTS)
                }
                _ => 0,
            };
            total
                .checked_add(2_usize.saturating_add(insertion_marks))
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
        })?;
        u32::try_from(overlay_instance_envelope)
            .map_err(|_| AlignmentSummaryGpuPrepareError::TooManyOverlayPrimitives)?;
        let uniform_envelope = semantic_draw_envelope
            .checked_add(overlay_draw_envelope)
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        if uniform_envelope > 0 {
            u32::try_from(
                (uniform_envelope - 1)
                    .checked_mul(self.uniform_stride)
                    .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?,
            )
            .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?;
        }
        let uniform_capacity = (uniform_envelope > 0)
            .then(|| next_summary_capacity(uniform_envelope))
            .transpose()?
            .unwrap_or(0);
        let uniform_bytes = uniform_capacity
            .checked_mul(self.uniform_stride)
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        let overlay_capacity = (overlay_instance_envelope > 0)
            .then(|| next_summary_capacity(overlay_instance_envelope))
            .transpose()?
            .unwrap_or(0);
        let overlay_bytes = overlay_capacity
            .checked_mul(size_of::<AlignmentSummaryGpuOverlayInstance>())
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        let maximum_frame_bytes = alignment_summary_gpu_frame_bytes(
            requests.len(),
            prepared_page_keys.len(),
            requests.len(),
            requests.len(),
            semantic_draw_envelope,
            overlay_draw_envelope,
            semantic_draw_envelope.saturating_add(4),
        );
        let mut frame_slots = Vec::with_capacity(ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT);
        for _ in 0..ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT {
            let uniform_slot = if uniform_bytes > 0 {
                let slot = take_summary_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    uniform_bytes,
                ));
                Some(slot)
            } else {
                None
            };
            let overlay_slot = if overlay_bytes > 0 {
                let slot = take_summary_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    overlay_bytes,
                ));
                Some(slot)
            } else {
                None
            };
            let frame_slot = take_summary_bundle_slot(&mut next_slot)?;
            demands.push(AllocationBundleDemand::new(
                frame_slot,
                AllocationClass::TransitionFrame,
                maximum_frame_bytes,
            ));
            frame_slots.push(PlannedSummaryFrameSlot {
                uniform_slot,
                overlay_slot,
                frame_slot,
            });
        }
        let frame_slots: [PlannedSummaryFrameSlot; ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT] =
            frame_slots
                .try_into()
                .expect("summary camera frame slot count is a compile-time constant");
        let uniform_payload_bytes = uniform_count
            .checked_mul(self.uniform_stride)
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        let overlay_payload_bytes = overlay_instances
            .checked_mul(size_of::<AlignmentSummaryGpuOverlayInstance>())
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        let missing_scratch = missing_pages.iter().try_fold(0_usize, |total, page| {
            total
                .checked_add(summary_page_maximum_packing_bytes(&page.request)?)
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
        })?;
        let transient_bytes = missing_scratch
            .checked_add(uniform_payload_bytes)
            .and_then(|total| total.checked_add(overlay_payload_bytes.saturating_mul(2)))
            .and_then(|total| {
                total.checked_add(mandatory_draws.saturating_mul(size_of::<PendingSummaryDraw>()))
            })
            .and_then(|total| {
                total.checked_add(mandatory_draws.saturating_mul(size_of::<PreparedSummaryDraw>()))
            })
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;

        Ok(AlignmentSummaryGpuReplacementPlan {
            policy_revision: ledger.policy_revision(),
            first_bundle_slot,
            next_bundle_slot: AllocationBundleSlot::new(next_slot),
            renderer_revision: self.revision,
            device_generation: self.device_generation,
            scene_revisions,
            scene_allocations,
            visible_pages,
            mandatory_keys,
            content_selectors,
            prepared_page_keys,
            missing_pages,
            demands,
            page_table_slot,
            page_table_requested_capacity,
            page_table_maximum_bytes,
            uniform_capacity,
            uniform_bytes,
            overlay_capacity,
            overlay_bytes,
            frame_slots,
            maximum_frame_bytes,
            request_count: requests.len(),
            mandatory_draws,
            overlay_draws,
            overlay_instances,
            page_cache_hits,
            page_cache_misses,
            transient_bytes,
        })
    }

    /// Consumes every prior per-lane summary frame and detaches only old-only cache objects for
    /// which the resulting token owns every ledger reference. Every resident page in the new
    /// candidate's complete prepared authority remains resident and is never credited as
    /// retirement, including prefetched pages outside the current visible camera.
    pub fn detach_replacement_retirement(
        &mut self,
        ledger: &AllocationLedger,
        plan: &AlignmentSummaryGpuReplacementPlan,
        prior_frames: &mut Vec<AlignmentSummaryGpuFrame>,
    ) -> Result<AlignmentSummaryGpuRetirement, AlignmentSummaryGpuReplacementError> {
        self.validate_replacement_plan(plan)?;
        if prior_frames
            .iter()
            .any(|frame| frame.device_generation != self.device_generation)
        {
            return Err(AlignmentSummaryGpuReplacementError::PriorFrameDeviceGeneration);
        }
        let mut prior_pin_counts = BTreeMap::<AllocationId, usize>::new();
        let mut counted_page_leases = BTreeSet::<usize>::new();
        for frame in prior_frames.iter() {
            for pin in &frame._pins {
                *prior_pin_counts.entry(pin.id()).or_default() += 1;
            }
            for lease in &frame._page_leases {
                if counted_page_leases.insert(Arc::as_ptr(lease) as usize) {
                    *prior_pin_counts.entry(lease.pin.id()).or_default() += 1;
                }
            }
        }
        let prepared = plan
            .prepared_page_keys
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut all_pages_retirable = true;
        for page in &self.cache.pages {
            let exact = page
                .allocation
                .as_ref()
                .map(|allocation| exact_summary_retirement_ownership(allocation, &prior_pin_counts))
                .transpose()?
                .unwrap_or(true);
            all_pages_retirable &= !prepared.contains(&page.key) && exact;
        }
        let can_retire_table = plan.page_table_slot.is_some()
            && all_pages_retirable
            && self
                .cache
                .page_table_allocation
                .as_ref()
                .map(|allocation| exact_summary_retirement_ownership(allocation, &prior_pin_counts))
                .transpose()?
                .unwrap_or(false);

        let mut claimable = BTreeSet::<AllocationId>::new();
        let mut retired_page_indices = Vec::new();
        if can_retire_table {
            for page in &self.cache.pages {
                if let Some(allocation) = &page.allocation {
                    claimable.insert(allocation.id());
                }
            }
        } else {
            for (index, page) in self.cache.pages.iter().enumerate() {
                if prepared.contains(&page.key)
                    || !page
                        .allocation
                        .as_ref()
                        .map(|allocation| {
                            exact_summary_retirement_ownership(allocation, &prior_pin_counts)
                        })
                        .transpose()?
                        .unwrap_or(true)
                {
                    continue;
                }
                if let Some(allocation) = &page.allocation {
                    claimable.insert(allocation.id());
                }
                retired_page_indices.push(index);
            }
        }
        if can_retire_table {
            if let Some(allocation) = &self.cache.page_table_allocation {
                claimable.insert(allocation.id());
            }
        }

        let mut retired_frame_buffer_indices = Vec::new();
        for (index, buffers) in self.frame_buffers.iter().enumerate() {
            let frame_references = prior_frames
                .iter()
                .filter(|frame| Arc::ptr_eq(&frame.buffers, buffers))
                .count();
            if Arc::strong_count(buffers) != frame_references.saturating_add(1) {
                continue;
            }
            let allocations = [
                buffers.uniform_allocation.as_ref(),
                buffers.overlay_allocation.as_ref(),
                Some(&buffers.frame_allocation),
            ];
            if allocations.iter().flatten().next().is_none()
                || !allocations
                    .iter()
                    .flatten()
                    .map(|allocation| {
                        exact_summary_retirement_ownership(allocation, &prior_pin_counts)
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .all(|exact| exact)
            {
                continue;
            }
            for allocation in allocations.into_iter().flatten() {
                claimable.insert(allocation.id());
            }
            retired_frame_buffer_indices.push(index);
        }
        // Build and validate the opaque claim while all renderer and caller ownership is still in
        // its original place. Every operation above and through `finish` is fallible; nothing has
        // been detached yet, so an error leaves the published frames and cache byte-for-byte
        // untouched.
        let mut builder = ledger.retirement_claim_builder();
        let mut included_page_leases = BTreeSet::<usize>::new();
        for frame in prior_frames.iter() {
            for pin in &frame._pins {
                if claimable.contains(&pin.id()) {
                    builder.include_pin(pin)?;
                }
            }
            for lease in &frame._page_leases {
                if included_page_leases.insert(Arc::as_ptr(lease) as usize)
                    && claimable.contains(&lease.pin.id())
                {
                    builder.include_pin(&lease.pin)?;
                }
            }
        }
        if can_retire_table {
            for page in &self.cache.pages {
                if let Some(allocation) = &page.allocation {
                    builder.include_lease(allocation)?;
                }
            }
        } else {
            for index in &retired_page_indices {
                if let Some(allocation) = &self.cache.pages[*index].allocation {
                    builder.include_lease(allocation)?;
                }
            }
        }
        if can_retire_table && let Some(allocation) = &self.cache.page_table_allocation {
            builder.include_lease(allocation)?;
        }
        for index in &retired_frame_buffer_indices {
            let buffers = &self.frame_buffers[*index];
            if let Some(allocation) = &buffers.uniform_allocation {
                builder.include_lease(allocation)?;
            }
            if let Some(allocation) = &buffers.overlay_allocation {
                builder.include_lease(allocation)?;
            }
            builder.include_lease(&buffers.frame_allocation)?;
        }
        let claim_set = builder.finish()?;

        // The commit point: all remaining operations are moves/removals and cannot fail.
        let retired_pages = if can_retire_table {
            std::mem::take(&mut self.cache.pages)
        } else {
            let mut retired = Vec::with_capacity(retired_page_indices.len());
            for index in retired_page_indices.into_iter().rev() {
                retired.push(self.cache.pages.remove(index));
            }
            retired.sort_unstable_by_key(|page| page.key);
            retired
        };
        let retired_page_table = if can_retire_table {
            let capacity = self.cache.page_table_capacity;
            self.cache.page_table_capacity = 0;
            self.cache
                .page_table_allocation
                .take()
                .map(|allocation| (allocation, capacity))
        } else {
            None
        };
        let mut retired_frame_buffers = Vec::with_capacity(retired_frame_buffer_indices.len());
        for index in retired_frame_buffer_indices.into_iter().rev() {
            retired_frame_buffers.push(RetiredSummaryFrameBuffer {
                index,
                buffers: self.frame_buffers.remove(index),
            });
        }
        retired_frame_buffers.sort_unstable_by_key(|retired| retired.index);
        self.rebuild_reclaim_index();
        Ok(AlignmentSummaryGpuRetirement {
            claim_set,
            prior_frames: std::mem::take(prior_frames),
            retired_pages,
            retired_page_table,
            retired_frame_buffers,
            renderer_revision: self.revision,
            device_generation: self.device_generation,
        })
    }

    /// Restores exactly the cache ownership detached before a refused bundle. The bundle must be
    /// dropped first so the ledger no longer holds this token's opaque retirement claim.
    pub fn restore_replacement_retirement(
        &mut self,
        mut retirement: AlignmentSummaryGpuRetirement,
    ) -> Result<Vec<AlignmentSummaryGpuFrame>, AlignmentSummaryGpuReplacementError> {
        if self.revision != retirement.renderer_revision
            || self.device_generation != retirement.device_generation
        {
            return Err(AlignmentSummaryGpuReplacementError::StaleRenderer {
                planned_revision: retirement.renderer_revision,
                current_revision: self.revision,
            });
        }
        if retirement.retired_page_table.is_some()
            && (self.cache.page_table_allocation.is_some() || !self.cache.pages.is_empty())
        {
            return Err(AlignmentSummaryGpuReplacementError::RetirementRestoreConflict);
        }
        if let Some((allocation, capacity)) = retirement.retired_page_table.take() {
            if !self.cache.pages.is_empty() {
                return Err(AlignmentSummaryGpuReplacementError::RetirementRestoreConflict);
            }
            // The token owns the original Vec allocation as well as its ledger lease. Move that
            // exact table back instead of copying entries into a different backing allocation.
            self.cache.pages = std::mem::take(&mut retirement.retired_pages);
            self.cache.page_table_allocation = Some(allocation);
            self.cache.page_table_capacity = capacity;
        } else {
            self.cache.pages.append(&mut retirement.retired_pages);
            self.cache.pages.sort_unstable_by_key(|page| page.key);
        }
        self.rebuild_reclaim_index();
        for retired in retirement.retired_frame_buffers.drain(..) {
            self.frame_buffers
                .insert(retired.index.min(self.frame_buffers.len()), retired.buffers);
        }
        Ok(std::mem::take(&mut retirement.prior_frames))
    }

    fn validate_replacement_plan(
        &self,
        plan: &AlignmentSummaryGpuReplacementPlan,
    ) -> Result<(), AlignmentSummaryGpuReplacementError> {
        if plan.device_generation != self.device_generation {
            return Err(AlignmentSummaryGpuReplacementError::DeviceGeneration {
                planned: plan.device_generation,
                current: self.device_generation,
            });
        }
        if plan.renderer_revision != self.revision {
            return Err(AlignmentSummaryGpuReplacementError::StaleRenderer {
                planned_revision: plan.renderer_revision,
                current_revision: self.revision,
            });
        }
        Ok(())
    }

    /// Builds all page, table, uniform, overlay, and frame objects under the coordinator's
    /// accepted bundle. Retained capacity is not reserved here and renderer-visible state is not
    /// mutated; only separately bounded upload scratch is acquired.
    pub fn stage_replacement(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        plan: AlignmentSummaryGpuReplacementPlan,
        bundle: &AllocationBundleReservation,
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentSummaryGpuStagedReplacement, AlignmentSummaryGpuReplacementError> {
        self.validate_replacement_plan(&plan)?;
        if bundle.policy_revision() != plan.policy_revision {
            return Err(AlignmentSummaryGpuReplacementError::BundlePolicyRevision {
                planned: plan.policy_revision,
                reserved: bundle.policy_revision(),
            });
        }
        if requests.len() != plan.request_count {
            return Err(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan);
        }
        for request in requests {
            validate_summary_request(request)?;
        }
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            plan.transient_bytes,
            ReservationKind::Transition,
        )?;
        let visible_pages = requests
            .iter()
            .map(|request| summary_requests_for_scene(request, true))
            .collect::<Result<Vec<_>, _>>()?;
        let content_selectors = summary_content_selectors(requests);
        if visible_pages.len() != plan.visible_pages.len()
            || requests
                .iter()
                .map(|request| request.scene.revision())
                .ne(plan.scene_revisions.iter().copied())
            || requests
                .iter()
                .map(|request| request.scene.metadata_allocation().id())
                .ne(plan.scene_allocations.iter().copied())
            || visible_pages
                .iter()
                .zip(&plan.visible_pages)
                .any(|(now, planned)| {
                    now.iter()
                        .map(|page| page.key)
                        .ne(planned.iter().map(|page| page.key))
                })
            || content_selectors != plan.content_selectors
        {
            return Err(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan);
        }
        for missing in &plan.missing_pages {
            if self.page_index(missing.request.key).is_some() {
                return Err(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan);
            }
        }
        validate_device_buffer_size(device, plan.uniform_bytes)?;
        validate_device_buffer_size(device, plan.overlay_bytes)?;

        let mut staged_pages = Vec::with_capacity(plan.missing_pages.len());
        let mut geometry_uploaded_bytes = 0_usize;
        for missing in &plan.missing_pages {
            let packed = PackedAlignmentSummaryPage::from_request(missing.request.borrowed())?;
            let instance_count = u32::try_from(packed.instance_count()).map_err(|_| {
                AlignmentSummaryGpuPageAdmissionError::TooManyInstances(packed.instance_count())
            })?;
            let buffer_capacity = packed_buffer_capacity(&packed)?;
            let charged_capacity = buffer_capacity
                .checked_add(packed.range_index_bytes())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            if charged_capacity > missing.maximum_bytes {
                return Err(AlignmentSummaryGpuReplacementError::PageCapacityExceeded {
                    key: missing.request.key,
                    actual: charged_capacity,
                    maximum: missing.maximum_bytes,
                });
            }
            validate_device_buffer_size(device, buffer_capacity)?;
            let buffer = (buffer_capacity > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV staged immutable alignment summary GPU page"),
                    size: buffer_capacity as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            if let Some(buffer) = &buffer {
                write_packed_page(queue, buffer, &packed);
            }
            geometry_uploaded_bytes = geometry_uploaded_bytes
                .checked_add(packed.uploaded_bytes())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            let metadata = match packed {
                PackedAlignmentSummaryPage::Depth { range_maximum, .. } => {
                    GpuSummaryPageMetadata::Depth(range_maximum)
                }
                PackedAlignmentSummaryPage::Alleles { .. } => GpuSummaryPageMetadata::Alleles,
                PackedAlignmentSummaryPage::Modifications { metadata, .. } => {
                    GpuSummaryPageMetadata::Modifications(metadata)
                }
                PackedAlignmentSummaryPage::Junctions { metadata, .. } => {
                    GpuSummaryPageMetadata::Junctions(metadata)
                }
            };
            staged_pages.push(StagedSummaryPage {
                key: missing.request.key,
                interval_start: missing.request.page.interval().start(),
                slot: missing.slot,
                buffer,
                instance_count,
                metadata,
                charged_capacity,
            });
        }

        let page_table = if plan.page_table_slot.is_some() {
            let candidate = Vec::with_capacity(plan.page_table_requested_capacity);
            let required = self.cache.pages.len().saturating_add(staged_pages.len());
            if candidate.capacity() < required {
                return Err(AlignmentSummaryGpuPageAdmissionError::PageTableCapacity {
                    required,
                    actual: candidate.capacity(),
                }
                .into());
            }
            let actual_bytes = summary_page_table_retained_bytes(candidate.capacity())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            if actual_bytes > plan.page_table_maximum_bytes {
                return Err(
                    AlignmentSummaryGpuReplacementError::PageTableCapacityExceeded {
                        actual: actual_bytes,
                        maximum: plan.page_table_maximum_bytes,
                    },
                );
            }
            Some(candidate)
        } else {
            None
        };
        let mut staged_buffers = Vec::with_capacity(ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT);
        for slot in plan.frame_slots {
            let uniform_buffer = (plan.uniform_bytes > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV staged alignment summary uniform frame slot"),
                    size: plan.uniform_bytes as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            let uniform_bind_group = uniform_buffer
                .as_ref()
                .map(|buffer| create_summary_bind_group(device, &self.uniform_layout, buffer));
            let overlay_buffer = (plan.overlay_bytes > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV staged alignment summary overlay frame slot"),
                    size: plan.overlay_bytes as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            staged_buffers.push(StagedSummaryFrameBuffers {
                uniform_buffer,
                uniform_bind_group,
                uniform_slot: slot.uniform_slot,
                uniform_capacity: plan.uniform_capacity,
                overlay_buffer,
                overlay_slot: slot.overlay_slot,
                overlay_capacity: plan.overlay_capacity,
                frame_slot: slot.frame_slot,
            });
        }

        let covered = requests
            .iter()
            .map(summary_covered_interval)
            .collect::<Vec<_>>();
        let mut resolved_depth_scales = Vec::with_capacity(requests.len());
        let mut range_maximum_queries = 0_usize;
        for ((request, pages), covered) in requests.iter().zip(&visible_pages).zip(&covered) {
            let fixed = shared_fixed_scale(&request.options)?;
            let maximum = if let Some(fixed) = fixed {
                fixed.max(1)
            } else if let Some(covered) = covered {
                let exact_start = request.camera.exact_start().max(covered.start() as f64);
                let exact_end = (request.camera.exact_start() + request.camera.span())
                    .min(covered.end() as f64);
                let mut maximum = 0_u32;
                for page_request in pages
                    .iter()
                    .filter(|page| page.key.family() == SummaryPageFamily::Depth)
                {
                    let metadata = self
                        .page(page_request.key)
                        .map(|page| &page.metadata)
                        .or_else(|| {
                            staged_pages
                                .iter()
                                .find(|page| page.key == page_request.key)
                                .map(|page| &page.metadata)
                        })
                        .ok_or(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan)?;
                    let GpuSummaryPageMetadata::Depth(index) = metadata else {
                        return Err(AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(
                            page_request.key,
                        )
                        .into());
                    };
                    maximum = maximum.max(index.maximum_depth_exact(exact_start, exact_end));
                    range_maximum_queries = range_maximum_queries.saturating_add(1);
                }
                maximum.max(1)
            } else {
                1
            };
            resolved_depth_scales.push(maximum);
        }
        let junction_metrics = resolve_junction_request_metrics(requests, &visible_pages)?;
        let junction_entries_examined = visible_pages
            .iter()
            .flatten()
            .filter(|page| page.key.family() == SummaryPageFamily::Junctions)
            .map(|page| page.page.primitives().len())
            .sum();

        let mut pending_draws = Vec::with_capacity(plan.mandatory_draws);
        let mut overlays = Vec::with_capacity(plan.overlay_instances);
        let mut pending_overlay_draws = Vec::with_capacity(plan.overlay_draws);
        let scene_revisions = plan.scene_revisions.to_vec();
        let scene_allocations = plan.scene_allocations.to_vec();
        let mut camera_sequences = Vec::with_capacity(requests.len());
        let mut depth_instances = 0_usize;
        let mut allele_instances = 0_usize;
        let mut junction_instances = 0_usize;
        let mut modification_instances = 0_usize;
        for ((((request, pages), covered), scale_maximum), junction_metrics) in requests
            .iter()
            .zip(&visible_pages)
            .zip(&covered)
            .zip(resolved_depth_scales.iter().copied())
            .zip(junction_metrics.iter().copied())
        {
            camera_sequences.push(request.camera_sequence);
            for page_request in pages {
                let (interval_start, instance_count, metadata) =
                    if let Some(page) = self.page(page_request.key) {
                        (page.interval_start, page.instance_count, &page.metadata)
                    } else {
                        let page = staged_pages
                            .iter()
                            .find(|page| page.key == page_request.key)
                            .ok_or(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan)?;
                        (page.interval_start, page.instance_count, &page.metadata)
                    };
                let style = summary_draw_style(
                    &request.options,
                    page_request.key,
                    metadata,
                    instance_count,
                )?;
                match page_request.key.family() {
                    SummaryPageFamily::Depth => {
                        depth_instances = depth_instances.saturating_add(instance_count as usize)
                    }
                    SummaryPageFamily::Alleles => {
                        allele_instances = allele_instances.saturating_add(instance_count as usize)
                    }
                    SummaryPageFamily::Modifications => {
                        modification_instances =
                            modification_instances.saturating_add(instance_count as usize)
                    }
                    SummaryPageFamily::Junctions => {
                        junction_instances =
                            junction_instances.saturating_add(instance_count as usize)
                    }
                    family => {
                        return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family).into());
                    }
                }
                pending_draws.push(PendingSummaryDraw {
                    page: page_request.key,
                    coherence_scope: request.coherence_scope,
                    lane_identity: request.lane_identity,
                    camera: request.camera,
                    screen_size: request.screen_size_points,
                    scissor: request.scissor,
                    page_start: interval_start,
                    covered_clip_x: summary_covered_clip_x(
                        request.camera,
                        covered.expect("visible pages imply covered scene authority"),
                    ),
                    scale_maximum,
                    minimum_alternate_fraction: style.minimum_alternate_fraction,
                    transform: style.transform,
                    quality_weighted: style.quality_weighted,
                    allele_interpretation: style.allele_interpretation,
                    hidden_modification_mask: style.hidden_modification_mask,
                    modification_color_mode: style.modification_color_mode,
                    junction_scale_maximum: junction_metrics
                        .map_or(1.0, |metrics| metrics.resolved_scale_maximum),
                    junction_normalization: style.junction_normalization,
                    junction_row_count: junction_metrics
                        .map_or(style.junction_row_count, |metrics| metrics.row_count),
                    minimum_junction_depth: style.minimum_junction_depth,
                    minimum_junction_flank: style.minimum_junction_flank,
                    junction_display_mode: style.junction_display_mode,
                    junction_count_mode: match &request.options {
                        AlignmentSummaryGpuLaneOptions::Sashimi(options) => {
                            options.projection.count_mode
                        }
                        AlignmentSummaryGpuLaneOptions::Coverage(_)
                        | AlignmentSummaryGpuLaneOptions::Modifications(_) => SashimiCountMode::Raw,
                    },
                    junction_label_mode: match &request.options {
                        AlignmentSummaryGpuLaneOptions::Sashimi(options) => {
                            options.projection.label_mode
                        }
                        AlignmentSummaryGpuLaneOptions::Coverage(_)
                        | AlignmentSummaryGpuLaneOptions::Modifications(_) => {
                            SashimiLabelMode::None
                        }
                    },
                    show_junction_labels: matches!(
                        &request.options,
                        AlignmentSummaryGpuLaneOptions::Sashimi(options)
                            if options.projection.show_labels
                                && options.projection.label_mode != SashimiLabelMode::None
                    ),
                    overlays: request.overlays,
                    background_range: style.background_range,
                    foreground_range: style.foreground_range,
                });
            }
            let start = u32::try_from(overlays.len())
                .expect("overlay address space was validated by the replacement plan");
            append_summary_overlays(request, &mut overlays);
            let end = u32::try_from(overlays.len())
                .expect("overlay address space was validated by the replacement plan");
            if start != end {
                pending_overlay_draws.push((
                    request.camera,
                    request.screen_size_points,
                    request.scissor,
                    start..end,
                ));
            }
        }
        if pending_draws.len() != plan.mandatory_draws
            || overlays.len() != plan.overlay_instances
            || pending_overlay_draws.len() != plan.overlay_draws
        {
            return Err(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan);
        }

        let uniform_payload_bytes = plan
            .mandatory_draws
            .saturating_add(plan.overlay_draws)
            .saturating_mul(self.uniform_stride);
        let mut uniforms = vec![0_u8; uniform_payload_bytes];
        let mut draws = Vec::with_capacity(plan.mandatory_draws);
        let mut prepared_overlays = Vec::with_capacity(plan.overlay_draws);
        let mut uniform_index = 0_usize;
        for request in &pending_draws {
            let uniform = summary_uniform(
                request.camera,
                request.screen_size,
                request.page_start,
                request.covered_clip_x,
                request.scale_maximum,
                request.minimum_alternate_fraction,
                request.transform,
                request.quality_weighted,
                request.allele_interpretation,
                request.page.family(),
                request.hidden_modification_mask,
                request.modification_color_mode,
                request.junction_scale_maximum,
                request.junction_normalization,
                request.junction_row_count,
                request.minimum_junction_depth,
                request.minimum_junction_flank,
                request.junction_display_mode,
                request.show_junction_labels,
                request.overlays,
            );
            let offset = uniform_index.saturating_mul(self.uniform_stride);
            uniforms[offset..offset + size_of::<AlignmentSummaryGpuUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
            draws.push(PreparedSummaryDraw {
                page: request.page,
                coherence_scope: request.coherence_scope,
                lane_identity: request.lane_identity,
                camera: request.camera,
                page_start: request.page_start,
                junction_scale_maximum: request.junction_scale_maximum,
                junction_normalization: request.junction_normalization,
                junction_row_count: request.junction_row_count,
                minimum_junction_depth: request.minimum_junction_depth,
                minimum_junction_flank: request.minimum_junction_flank,
                junction_display_mode: request.junction_display_mode,
                junction_count_mode: request.junction_count_mode,
                junction_label_mode: request.junction_label_mode,
                show_junction_labels: request.show_junction_labels,
                uniform_offset: u32::try_from(offset)
                    .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?,
                scissor: request.scissor,
                background_range: request.background_range.clone(),
                foreground_range: request.foreground_range.clone(),
            });
            uniform_index = uniform_index.saturating_add(1);
        }
        for (camera, screen_size, scissor, range) in pending_overlay_draws {
            let uniform = summary_uniform(
                camera,
                screen_size,
                0,
                [
                    camera.clip().x as f32,
                    (camera.clip().x + camera.clip().width) as f32,
                ],
                1,
                0.0,
                CoverageScaleTransform::Linear,
                false,
                CoverageAlleleInterpretation::GenericReferenceAware,
                SummaryPageFamily::Depth,
                0,
                ModifiedBaseColorMode::ByType,
                1.0,
                1.0,
                1,
                0,
                0,
                JunctionDisplayMode::Collapsed,
                false,
                AlignmentSummaryFrameOverlays::default(),
            );
            let offset = uniform_index.saturating_mul(self.uniform_stride);
            uniforms[offset..offset + size_of::<AlignmentSummaryGpuUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
            prepared_overlays.push(PreparedSummaryOverlayDraw {
                uniform_offset: u32::try_from(offset)
                    .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?,
                scissor,
                range,
            });
            uniform_index = uniform_index.saturating_add(1);
        }
        let initial_buffers = staged_buffers
            .first()
            .expect("replacement provisions at least one summary camera frame slot");
        if let Some(buffer) = &initial_buffers.uniform_buffer
            && !uniforms.is_empty()
        {
            queue.write_buffer(buffer, 0, &uniforms);
        }
        if let Some(buffer) = &initial_buffers.overlay_buffer
            && !overlays.is_empty()
        {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&overlays));
        }

        let mut existing_pins = Vec::with_capacity(plan.mandatory_draws.saturating_add(1));
        let staged_keys = staged_pages
            .iter()
            .map(|page| page.key)
            .collect::<BTreeSet<_>>();
        for draw in &draws {
            if staged_keys.contains(&draw.page) {
                continue;
            }
            let page = self
                .page(draw.page)
                .ok_or(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan)?;
            if let Some(allocation) = &page.allocation {
                existing_pins.push(allocation.pin()?);
            }
        }
        if plan.page_table_slot.is_none()
            && let Some(allocation) = &self.cache.page_table_allocation
        {
            existing_pins.push(allocation.pin()?);
        }
        let new_page_pins = staged_pages
            .iter()
            .filter(|page| page.charged_capacity > 0)
            .count();
        let new_buffer_pins = usize::from(plan.frame_slots[0].uniform_slot.is_some())
            .saturating_add(usize::from(plan.frame_slots[0].overlay_slot.is_some()));
        let retained_frame_bytes = alignment_summary_gpu_frame_bytes(
            scene_revisions.len(),
            plan.prepared_page_keys.len(),
            camera_sequences.len(),
            resolved_depth_scales.len(),
            draws.len(),
            prepared_overlays.len(),
            existing_pins
                .len()
                .saturating_add(new_page_pins)
                .saturating_add(new_buffer_pins)
                .saturating_add(usize::from(plan.page_table_slot.is_some()))
                .saturating_add(1),
        );
        if retained_frame_bytes > plan.maximum_frame_bytes {
            return Err(AlignmentSummaryGpuReplacementError::FrameCapacityExceeded {
                actual: retained_frame_bytes,
                maximum: plan.maximum_frame_bytes,
            });
        }
        let mut actuals = staged_pages
            .iter()
            .map(|page| {
                AllocationBundleActual::new(page.slot, page.charged_capacity, publication_revision)
            })
            .collect::<Vec<_>>();
        if let (Some(slot), Some(table)) = (plan.page_table_slot, page_table.as_ref()) {
            actuals.push(AllocationBundleActual::new(
                slot,
                summary_page_table_retained_bytes(table.capacity()).unwrap_or(usize::MAX),
                publication_revision,
            ));
        }
        for slot in plan.frame_slots {
            if let Some(uniform_slot) = slot.uniform_slot {
                actuals.push(AllocationBundleActual::new(
                    uniform_slot,
                    plan.uniform_bytes,
                    publication_revision,
                ));
            }
            if let Some(overlay_slot) = slot.overlay_slot {
                actuals.push(AllocationBundleActual::new(
                    overlay_slot,
                    plan.overlay_bytes,
                    publication_revision,
                ));
            }
            actuals.push(AllocationBundleActual::new(
                slot.frame_slot,
                plan.maximum_frame_bytes,
                publication_revision,
            ));
        }
        actuals.sort_unstable_by_key(|actual| actual.slot);
        let admission = AlignmentSummaryGpuBatchAdmission {
            requested_pages: plan.mandatory_keys.len(),
            admitted_pages: plan.missing_pages.len(),
            already_resident_pages: plan
                .mandatory_keys
                .len()
                .saturating_sub(plan.missing_pages.len()),
            instances: staged_pages.iter().fold(0_usize, |total, page| {
                total.saturating_add(page.instance_count as usize)
            }),
            uploaded_bytes: geometry_uploaded_bytes,
        };
        Ok(AlignmentSummaryGpuStagedReplacement {
            pages: staged_pages,
            page_table,
            buffers: staged_buffers,
            scene_revisions,
            scene_allocations,
            camera_sequences,
            resolved_depth_scales,
            draws,
            overlay_draws: prepared_overlays,
            existing_pins,
            actuals,
            admission,
            depth_instances,
            allele_instances,
            junction_instances,
            modification_instances,
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes,
            range_maximum_queries,
            junction_entries_examined,
            plan,
        })
    }

    /// Validates every renderer/cache and demand/actual invariant while the old retirement token
    /// is still recoverable. A coordinator should preflight both renderers before bundle commit.
    pub fn preflight_replacement_publication(
        &self,
        staged: AlignmentSummaryGpuStagedReplacement,
    ) -> Result<AlignmentSummaryGpuReadyReplacement, AlignmentSummaryGpuReplacementError> {
        self.validate_replacement_plan(&staged.plan)?;
        if staged.actuals.len() != staged.plan.demands.len() {
            return Err(AlignmentSummaryGpuReplacementError::ActualDemandMismatch);
        }
        for demand in &staged.plan.demands {
            let matches = staged
                .actuals
                .iter()
                .filter(|actual| actual.slot == demand.slot)
                .collect::<Vec<_>>();
            if matches.len() != 1 || matches[0].charged_capacity > demand.maximum_bytes {
                return Err(AlignmentSummaryGpuReplacementError::ActualDemandMismatch);
            }
        }
        let staged_keys = staged
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<BTreeSet<_>>();
        if staged_keys.len() != staged.pages.len()
            || staged
                .plan
                .mandatory_keys
                .iter()
                .any(|key| !staged_keys.contains(key) && self.page_index(*key).is_none())
            || staged.page_table.is_some() != staged.plan.page_table_slot.is_some()
            || staged.buffers.len() != ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT
            || staged
                .buffers
                .iter()
                .zip(staged.plan.frame_slots)
                .any(|(buffers, slot)| {
                    buffers.uniform_buffer.is_some() != slot.uniform_slot.is_some()
                        || buffers.overlay_buffer.is_some() != slot.overlay_slot.is_some()
                        || buffers.frame_slot != slot.frame_slot
                })
        {
            return Err(AlignmentSummaryGpuReplacementError::RequestChangedAfterPlan);
        }
        Ok(AlignmentSummaryGpuReadyReplacement { staged })
    }

    /// Partitions this renderer's leases from the exact vector minted by bundle commit. Commit
    /// already validated every staged actual, so after settlement a missing/duplicate slot is a
    /// coordinator invariant violation rather than a recoverable publication failure.
    pub fn bind_replacement_leases(
        &self,
        ready: AlignmentSummaryGpuReadyReplacement,
        settled_leases: &mut Vec<AllocationBundleLease>,
    ) -> AlignmentSummaryGpuPublishableReplacement {
        assert_eq!(self.device_generation, ready.staged.plan.device_generation);
        assert_eq!(self.revision, ready.staged.plan.renderer_revision);
        let staged = &ready.staged;
        let required_slots = staged
            .actuals
            .iter()
            .filter(|actual| actual.charged_capacity > 0)
            .map(|actual| actual.slot)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            required_slots.len(),
            staged
                .actuals
                .iter()
                .filter(|actual| actual.charged_capacity > 0)
                .count(),
            "preflighted summary actual slots are unique"
        );
        for slot in &required_slots {
            let matches = settled_leases
                .iter()
                .filter(|lease| lease.slot == *slot)
                .count();
            assert_eq!(
                matches, 1,
                "settled bundle must mint each summary slot once"
            );
        }
        let mut leases = Vec::with_capacity(required_slots.len());
        for slot in required_slots {
            let index = settled_leases
                .iter()
                .position(|lease| lease.slot == slot)
                .expect("settled summary slots were validated before mutation");
            leases.push(settled_leases.remove(index));
        }
        AlignmentSummaryGpuPublishableReplacement {
            staged: ready.staged,
            leases,
        }
    }

    /// Atomically exposes a preflighted candidate. Bundle settlement minted every lease in this
    /// move-only object, so an unchanged renderer leaves no recoverable error path here.
    pub fn publish_replacement(
        &mut self,
        publishable: AlignmentSummaryGpuPublishableReplacement,
    ) -> (
        AlignmentSummaryGpuFrame,
        AlignmentSummaryGpuPrepareStats,
        AlignmentSummaryGpuBatchAdmission,
    ) {
        let AlignmentSummaryGpuPublishableReplacement {
            mut staged,
            mut leases,
        } = publishable;
        assert_eq!(self.device_generation, staged.plan.device_generation);
        assert_eq!(self.revision, staged.plan.renderer_revision);
        let mut take_lease = |slot: AllocationBundleSlot| {
            let index = leases
                .iter()
                .position(|lease| lease.slot == slot)
                .expect("preflighted summary publication owns every non-zero slot");
            leases.remove(index).allocation
        };
        let next_revision = staged.plan.renderer_revision.saturating_add(1);
        let mut new_pages = Vec::with_capacity(staged.pages.len());
        let mut pins = std::mem::take(&mut staged.existing_pins);
        for page in staged.pages {
            let allocation = if page.charged_capacity > 0 {
                let allocation = take_lease(page.slot);
                pins.push(
                    allocation
                        .pin()
                        .expect("settled summary page lease remains live until publication"),
                );
                Some(allocation)
            } else {
                None
            };
            new_pages.push(GpuSummaryPage {
                key: page.key,
                interval_start: page.interval_start,
                buffer: page.buffer,
                instance_count: page.instance_count,
                metadata: page.metadata,
                allocation,
                last_used_revision: next_revision,
            });
        }
        let mut frame_buffers = Vec::with_capacity(staged.buffers.len());
        for buffers in staged.buffers {
            let uniform_allocation = buffers.uniform_slot.map(&mut take_lease);
            let overlay_allocation = buffers.overlay_slot.map(&mut take_lease);
            let frame_allocation = take_lease(buffers.frame_slot);
            frame_buffers.push(Arc::new(GpuSummaryFrameBuffers {
                uniform_buffer: buffers.uniform_buffer,
                uniform_bind_group: buffers.uniform_bind_group,
                uniform_allocation,
                uniform_capacity: buffers.uniform_capacity,
                overlay_buffer: buffers.overlay_buffer,
                overlay_allocation,
                overlay_capacity: buffers.overlay_capacity,
                frame_allocation,
                frame_capacity: staged.plan.maximum_frame_bytes,
                capture_ring: true,
            }));
        }
        let initial_frame_buffers = Arc::clone(
            frame_buffers
                .first()
                .expect("replacement provisions at least one summary camera frame slot"),
        );
        if let Some(allocation) = &initial_frame_buffers.uniform_allocation {
            pins.push(
                allocation
                    .pin()
                    .expect("settled summary uniform lease remains live until publication"),
            );
        }
        if let Some(allocation) = &initial_frame_buffers.overlay_allocation {
            pins.push(
                allocation
                    .pin()
                    .expect("settled summary overlay lease remains live until publication"),
            );
        }
        let new_page_table_allocation = staged.plan.page_table_slot.map(&mut take_lease);
        if let Some(allocation) = &new_page_table_allocation {
            pins.push(
                allocation
                    .pin()
                    .expect("settled summary table lease remains live until publication"),
            );
        }
        pins.push(
            initial_frame_buffers
                .frame_allocation
                .pin()
                .expect("settled summary frame lease remains live until publication"),
        );
        debug_assert!(leases.is_empty());

        if let Some(mut page_table) = staged.page_table.take() {
            page_table.extend(std::mem::take(&mut self.cache.pages));
            page_table.append(&mut new_pages);
            page_table.sort_unstable_by_key(|page| page.key);
            self.cache.pages = page_table;
            self.cache.page_table_capacity = self.cache.pages.capacity();
            self.cache.page_table_allocation = new_page_table_allocation;
        } else {
            debug_assert!(new_page_table_allocation.is_none());
            self.cache.pages.append(&mut new_pages);
            self.cache.pages.sort_unstable_by_key(|page| page.key);
        }
        for draw in &staged.draws {
            if let Some(page) = self.page_mut(draw.page) {
                page.last_used_revision = next_revision;
            }
        }
        self.rebuild_reclaim_index();
        self.frame_buffers.append(&mut frame_buffers);
        self.revision = next_revision;
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_add(staged.geometry_uploaded_bytes);
        let content_authority = AlignmentSummaryGpuContentAuthority::new(
            self.device_generation,
            staged.scene_revisions.clone().into(),
            staged.scene_allocations.into(),
            staged.plan.content_selectors.clone().into(),
            Arc::clone(&staged.plan.prepared_page_keys),
        );
        let frame = AlignmentSummaryGpuFrame {
            revision: next_revision,
            device_generation: self.device_generation,
            scene_revisions: staged.scene_revisions.into(),
            camera_sequences: staged.camera_sequences.into(),
            resolved_depth_scales: staged.resolved_depth_scales.into(),
            content_authority,
            draws: staged.draws.into(),
            overlay_draws: staged.overlay_draws.into(),
            buffers: initial_frame_buffers,
            _page_leases: Vec::new(),
            _pins: pins,
        };
        let resident_gpu_bytes = self
            .resident_retained_bytes()
            .expect("published summary leases remain registered in their ledger");
        let stats = AlignmentSummaryGpuPrepareStats {
            scenes: staged.plan.request_count,
            mandatory_pages: staged.plan.mandatory_draws,
            depth_instances: staged.depth_instances,
            allele_instances: staged.allele_instances,
            junction_instances: staged.junction_instances,
            modification_instances: staged.modification_instances,
            overlay_instances: staged.plan.overlay_instances,
            camera_uniform_bytes: staged.camera_uniform_bytes,
            geometry_uploaded_bytes: std::mem::take(&mut self.pending_geometry_uploaded_bytes),
            range_maximum_queries: staged.range_maximum_queries,
            junction_entries_examined: staged.junction_entries_examined,
            page_pin_acquisitions: staged.plan.mandatory_draws,
            page_cache_hits: staged.plan.page_cache_hits,
            page_cache_misses: staged.plan.page_cache_misses,
            resident_pages: self.cache.pages.len(),
            resident_gpu_bytes,
        };
        (frame, stats, staged.admission)
    }

    pub fn admit_page(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        request: AlignmentSummaryGpuPageRequest<'_>,
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentSummaryGpuPageAdmission, AlignmentSummaryGpuPageAdmissionError> {
        let admission = self.admit_pages(
            device,
            queue,
            std::slice::from_ref(&request),
            ledger,
            publication_revision,
        )?;
        if admission.admitted_pages == 0 {
            Ok(AlignmentSummaryGpuPageAdmission::AlreadyResident)
        } else {
            Ok(AlignmentSummaryGpuPageAdmission::Admitted {
                instances: admission.instances,
                uploaded_bytes: admission.uploaded_bytes,
            })
        }
    }

    /// Transactionally admits a complete mandatory page set. Validation and packing finish under
    /// a charged transient reservation before the resident page table changes.
    pub fn admit_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[AlignmentSummaryGpuPageRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentSummaryGpuBatchAdmission, AlignmentSummaryGpuPageAdmissionError> {
        self.admit_pages_protecting(
            device,
            queue,
            pages,
            pages,
            ledger,
            publication_revision,
            ReservationKind::Normal,
        )
    }

    /// Admits a complete visible candidate while the prior frame may still pin its own visible
    /// pages. The explicit transition reservation is the renderer-side primitive used by atomic
    /// frame swaps; speculative prefetch continues to use the normal budget.
    pub fn admit_mandatory_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[AlignmentSummaryGpuPageRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentSummaryGpuBatchAdmission, AlignmentSummaryGpuPageAdmissionError> {
        self.admit_pages_protecting(
            device,
            queue,
            pages,
            pages,
            ledger,
            publication_revision,
            ReservationKind::Transition,
        )
    }

    fn admit_pages_protecting(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[AlignmentSummaryGpuPageRequest<'_>],
        protected: &[AlignmentSummaryGpuPageRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
        reservation_kind: ReservationKind,
    ) -> Result<AlignmentSummaryGpuBatchAdmission, AlignmentSummaryGpuPageAdmissionError> {
        let mut keys = Vec::with_capacity(pages.len());
        let mut resident_pages = 0_usize;
        let mut scratch_bytes = pages
            .len()
            .saturating_mul(size_of::<PendingSummaryPageAdmission>());
        for page in pages {
            let key = page.key()?;
            if keys.contains(&key) {
                return Err(AlignmentSummaryGpuPageAdmissionError::DuplicatePageKey(key));
            }
            keys.push(key);
            if self.page_index(key).is_some() {
                resident_pages = resident_pages.saturating_add(1);
            } else {
                let maximum = match key.family() {
                    SummaryPageFamily::Depth => page.page.primitives().len(),
                    SummaryPageFamily::Alleles => page.page.primitives().len().saturating_mul(4),
                    SummaryPageFamily::Modifications => {
                        page.page.primitives().len().saturating_mul(3)
                    }
                    SummaryPageFamily::Junctions => page.page.primitives().len().saturating_mul(4),
                    family => {
                        return Err(AlignmentSummaryGpuPageAdmissionError::Page(
                            AlignmentSummaryGpuPageError::UnsupportedFamily(family),
                        ));
                    }
                };
                scratch_bytes = scratch_bytes.saturating_add(
                    maximum.saturating_mul(size_of::<AlignmentSummaryGpuAlleleInstance>()),
                );
            }
        }
        if resident_pages == pages.len() {
            self.revision = self.revision.saturating_add(1);
            let revision = self.revision;
            for key in keys {
                if let Some(index) = self.page_index(key) {
                    self.touch_page_at(index, revision);
                }
            }
            return Ok(AlignmentSummaryGpuBatchAdmission {
                requested_pages: pages.len(),
                admitted_pages: 0,
                already_resident_pages: resident_pages,
                instances: 0,
                uploaded_bytes: 0,
            });
        }

        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            scratch_bytes,
            reservation_kind,
        )?;
        let mut pending = Vec::with_capacity(pages.len().saturating_sub(resident_pages));
        let mut retained_bytes = 0_usize;
        for page in pages {
            let key = page.key()?;
            if self.page_index(key).is_some() {
                continue;
            }
            let packed = PackedAlignmentSummaryPage::from_request(*page)?;
            let instance_count = packed.instance_count();
            let buffer_capacity = packed_buffer_capacity(&packed)?;
            validate_device_buffer_size(device, buffer_capacity)?;
            let retained_capacity = buffer_capacity.saturating_add(packed.range_index_bytes());
            retained_bytes = retained_bytes.saturating_add(retained_capacity);
            pending.push(PendingSummaryPageAdmission {
                key,
                instance_count: u32::try_from(instance_count).map_err(|_| {
                    AlignmentSummaryGpuPageAdmissionError::TooManyInstances(instance_count)
                })?,
                buffer_capacity,
                retained_capacity,
                packed,
            });
        }

        let protected_keys = protected
            .iter()
            .map(|page| page.key())
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = ledger.progress()?;
        let reservation_limit = match reservation_kind {
            ReservationKind::Normal => snapshot.budget,
            ReservationKind::Transition => {
                snapshot.budget.saturating_add(snapshot.transition_reserve)
            }
        };
        self.evict_unpinned_pages_for_excluding(
            ledger,
            retained_bytes,
            reservation_limit,
            &protected_keys,
        )?;
        let mut retained =
            ledger.reserve(AllocationClass::GpuPage, retained_bytes, reservation_kind)?;
        // Hold the page bytes while reserving/growing the table so aggregate transition capacity
        // is proven before either allocation becomes resident. Otherwise the table could grow
        // successfully and strand enough budget that this candidate, and every identical retry,
        // fails while leaving no published frame.
        self.ensure_page_table_capacity(
            ledger,
            self.cache.pages.len().saturating_add(pending.len()),
            reservation_kind,
        )?;
        self.revision = self.revision.saturating_add(1);
        let revision = self.revision;
        let mut admitted = Vec::with_capacity(pending.len());
        let mut uploaded_bytes = 0_usize;
        let admitted_pages = pending.len();
        let admitted_instances = pending.iter().fold(0_usize, |total, page| {
            total.saturating_add(page.instance_count as usize)
        });
        for candidate in pending {
            let buffer = (candidate.buffer_capacity > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV immutable alignment summary GPU page"),
                    size: candidate.buffer_capacity as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            if let Some(buffer) = &buffer {
                write_packed_page(queue, buffer, &candidate.packed);
            }
            uploaded_bytes = uploaded_bytes.saturating_add(candidate.packed.uploaded_bytes());
            let metadata = match candidate.packed {
                PackedAlignmentSummaryPage::Depth { range_maximum, .. } => {
                    GpuSummaryPageMetadata::Depth(range_maximum)
                }
                PackedAlignmentSummaryPage::Alleles { .. } => GpuSummaryPageMetadata::Alleles,
                PackedAlignmentSummaryPage::Modifications { metadata, .. } => {
                    GpuSummaryPageMetadata::Modifications(metadata)
                }
                PackedAlignmentSummaryPage::Junctions { metadata, .. } => {
                    GpuSummaryPageMetadata::Junctions(metadata)
                }
            };
            let allocation = (candidate.retained_capacity > 0)
                .then(|| retained.register(candidate.retained_capacity, publication_revision))
                .transpose()?;
            admitted.push(GpuSummaryPage {
                key: candidate.key,
                interval_start: page_interval_start(pages, candidate.key)
                    .expect("admitted key belongs to one validated request"),
                buffer,
                instance_count: candidate.instance_count,
                metadata,
                allocation,
                last_used_revision: revision,
            });
        }
        for key in keys {
            if let Some(index) = self.page_index(key) {
                self.cache.pages[index].last_used_revision = revision;
            }
        }
        self.cache.pages.extend(admitted);
        self.cache.pages.sort_unstable_by_key(|page| page.key);
        self.rebuild_reclaim_index();
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_add(uploaded_bytes);
        Ok(AlignmentSummaryGpuBatchAdmission {
            requested_pages: pages.len(),
            admitted_pages,
            already_resident_pages: resident_pages,
            instances: admitted_instances,
            uploaded_bytes,
        })
    }

    /// Captures a frame from already-resident immutable summary pages. Automatic scaling uses
    /// the per-page range indices built at admission, so no semantic primitive list is revisited.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
    ) -> Result<
        (AlignmentSummaryGpuFrame, AlignmentSummaryGpuPrepareStats),
        AlignmentSummaryGpuPrepareError,
    > {
        for request in requests {
            validate_summary_request(request)?;
        }
        let page_manifest = Self::prepared_page_keys(requests)?;
        self.record_content_manifest_build(page_manifest.len());
        let authority = AlignmentSummaryGpuContentAuthority::new(
            self.device_generation,
            requests
                .iter()
                .map(|request| request.scene.revision())
                .collect::<Vec<_>>()
                .into(),
            requests
                .iter()
                .map(|request| request.scene.metadata_allocation().id())
                .collect::<Vec<_>>()
                .into(),
            summary_content_selectors(requests).into(),
            page_manifest.into(),
        );
        self.capture_resident_camera_frame(device, queue, requests, ledger, &authority, true)
    }

    /// Builds a camera/options frame over a previously minted immutable summary authority. It is
    /// structurally unable to enumerate the prepared content manifest, admit/retire pages, or
    /// open a replacement bundle. Slot growth is reserved for initial publication only.
    fn capture_resident_camera_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        content_authority: &AlignmentSummaryGpuContentAuthority,
        allow_slot_growth: bool,
    ) -> Result<
        (AlignmentSummaryGpuFrame, AlignmentSummaryGpuPrepareStats),
        AlignmentSummaryGpuPrepareError,
    > {
        for request in requests {
            validate_summary_request(request)?;
        }
        let prepared_page_count = content_authority.page_manifest.len();
        let covered = requests
            .iter()
            .map(summary_covered_interval)
            .collect::<Vec<_>>();
        let visible = requests
            .iter()
            .map(|request| summary_requests_for_scene(request, true))
            .collect::<Result<Vec<_>, _>>()?;
        let mandatory_pages = visible.iter().map(Vec::len).sum::<usize>();
        let mut missing = 0_usize;
        let mut page_cache_hits = 0_usize;
        let mut depth_instances = 0_usize;
        let mut allele_instances = 0_usize;
        let mut junction_instances = 0_usize;
        let mut modification_instances = 0_usize;
        for pages in &visible {
            for request in pages {
                let Some(page) = self.page(request.key) else {
                    missing = missing.saturating_add(1);
                    continue;
                };
                page_cache_hits = page_cache_hits.saturating_add(1);
                match request.key.family() {
                    SummaryPageFamily::Depth => {
                        depth_instances =
                            depth_instances.saturating_add(page.instance_count as usize);
                    }
                    SummaryPageFamily::Alleles => {
                        allele_instances =
                            allele_instances.saturating_add(page.instance_count as usize);
                    }
                    SummaryPageFamily::Modifications => {
                        modification_instances =
                            modification_instances.saturating_add(page.instance_count as usize);
                    }
                    SummaryPageFamily::Junctions => {
                        junction_instances =
                            junction_instances.saturating_add(page.instance_count as usize);
                    }
                    family => {
                        return Err(AlignmentSummaryGpuPrepareError::Page(
                            AlignmentSummaryGpuPageError::UnsupportedFamily(family),
                        ));
                    }
                }
            }
        }
        if missing > 0 {
            return Err(AlignmentSummaryGpuPrepareError::MissingMandatoryPages { missing });
        }

        let mut resolved_depth_scales = Vec::with_capacity(requests.len());
        let mut range_maximum_queries = 0_usize;
        for ((request, pages), covered) in requests.iter().zip(&visible).zip(&covered) {
            let fixed = shared_fixed_scale(&request.options)?;
            let maximum = if let Some(fixed) = fixed {
                fixed.max(1)
            } else if let Some(covered) = covered {
                let exact_start = request.camera.exact_start().max(covered.start() as f64);
                let exact_end = (request.camera.exact_start() + request.camera.span())
                    .min(covered.end() as f64);
                let mut maximum = 0_u32;
                for page_request in pages
                    .iter()
                    .filter(|page| page.key.family() == SummaryPageFamily::Depth)
                {
                    let page = self
                        .page(page_request.key)
                        .expect("mandatory depth residency was proven above");
                    let GpuSummaryPageMetadata::Depth(index) = &page.metadata else {
                        return Err(AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(
                            page_request.key,
                        ));
                    };
                    maximum = maximum.max(index.maximum_depth_exact(exact_start, exact_end));
                    range_maximum_queries = range_maximum_queries.saturating_add(1);
                }
                maximum.max(1)
            } else {
                1
            };
            resolved_depth_scales.push(maximum);
        }
        let (junction_metrics, junction_range_queries, junction_entries_examined) =
            resolve_resident_junction_request_metrics(self, requests, &visible)?;
        range_maximum_queries = range_maximum_queries.saturating_add(junction_range_queries);

        let overlay_count = requests.iter().map(summary_overlay_count).sum::<usize>();
        u32::try_from(overlay_count)
            .map_err(|_| AlignmentSummaryGpuPrepareError::TooManyOverlayPrimitives)?;
        let overlay_draws = requests
            .iter()
            .filter(|request| summary_overlay_count(request) > 0)
            .count();
        let uniform_count = mandatory_pages.saturating_add(overlay_draws);
        let uniform_bytes = uniform_count.saturating_mul(self.uniform_stride);
        if uniform_count > 0 {
            u32::try_from((uniform_count - 1).saturating_mul(self.uniform_stride))
                .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?;
        }
        let overlay_bytes =
            overlay_count.saturating_mul(size_of::<AlignmentSummaryGpuOverlayInstance>());
        let scratch_bytes = uniform_bytes
            .saturating_add(overlay_bytes.saturating_mul(2))
            .saturating_add(mandatory_pages.saturating_mul(size_of::<PendingSummaryDraw>()))
            .saturating_add(mandatory_pages.saturating_mul(size_of::<PreparedSummaryDraw>()))
            .saturating_add(overlay_draws.saturating_mul(size_of::<PreparedSummaryOverlayDraw>()))
            .saturating_add(
                requests.len().saturating_mul(
                    size_of::<SceneRevision>()
                        .saturating_add(size_of::<u64>())
                        .saturating_add(size_of::<u32>()),
                ),
            );
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            scratch_bytes,
            ReservationKind::Transition,
        )?;
        let maximum_frame_bytes = alignment_summary_gpu_frame_bytes(
            requests.len(),
            prepared_page_count,
            requests.len(),
            requests.len(),
            mandatory_pages,
            overlay_draws,
            mandatory_pages.saturating_add(4),
        );
        let frame_buffers = if allow_slot_growth {
            self.acquire_frame_buffers(
                device,
                ledger,
                uniform_count,
                overlay_count,
                maximum_frame_bytes,
            )?
        } else {
            self.acquire_existing_frame_buffers(uniform_count, overlay_count, maximum_frame_bytes)?
        };

        let mut pending_draws = Vec::with_capacity(mandatory_pages);
        let mut overlays = Vec::with_capacity(overlay_count);
        let mut pending_overlay_draws = Vec::with_capacity(overlay_draws);
        let mut scene_revisions = Vec::with_capacity(requests.len());
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for ((((request, pages), covered), scale_maximum), junction_metrics) in requests
            .iter()
            .zip(&visible)
            .zip(&covered)
            .zip(resolved_depth_scales.iter().copied())
            .zip(junction_metrics.iter().copied())
        {
            scene_revisions.push(request.scene.revision());
            camera_sequences.push(request.camera_sequence);
            for page_request in pages {
                let page = self
                    .page(page_request.key)
                    .expect("mandatory summary residency was proven above");
                let style = summary_draw_style(
                    &request.options,
                    page_request.key,
                    &page.metadata,
                    page.instance_count,
                )?;
                pending_draws.push(PendingSummaryDraw {
                    page: page_request.key,
                    coherence_scope: request.coherence_scope,
                    lane_identity: request.lane_identity,
                    camera: request.camera,
                    screen_size: request.screen_size_points,
                    scissor: request.scissor,
                    page_start: page.interval_start,
                    covered_clip_x: summary_covered_clip_x(
                        request.camera,
                        covered.expect("visible summary pages require covered scene authority"),
                    ),
                    scale_maximum,
                    minimum_alternate_fraction: style.minimum_alternate_fraction,
                    transform: style.transform,
                    quality_weighted: style.quality_weighted,
                    allele_interpretation: style.allele_interpretation,
                    hidden_modification_mask: style.hidden_modification_mask,
                    modification_color_mode: style.modification_color_mode,
                    junction_scale_maximum: junction_metrics
                        .map_or(1.0, |metrics| metrics.resolved_scale_maximum),
                    junction_normalization: style.junction_normalization,
                    junction_row_count: junction_metrics
                        .map_or(style.junction_row_count, |metrics| metrics.row_count),
                    minimum_junction_depth: style.minimum_junction_depth,
                    minimum_junction_flank: style.minimum_junction_flank,
                    junction_display_mode: style.junction_display_mode,
                    junction_count_mode: match &request.options {
                        AlignmentSummaryGpuLaneOptions::Sashimi(options) => {
                            options.projection.count_mode
                        }
                        AlignmentSummaryGpuLaneOptions::Coverage(_)
                        | AlignmentSummaryGpuLaneOptions::Modifications(_) => SashimiCountMode::Raw,
                    },
                    junction_label_mode: match &request.options {
                        AlignmentSummaryGpuLaneOptions::Sashimi(options) => {
                            options.projection.label_mode
                        }
                        AlignmentSummaryGpuLaneOptions::Coverage(_)
                        | AlignmentSummaryGpuLaneOptions::Modifications(_) => {
                            SashimiLabelMode::None
                        }
                    },
                    show_junction_labels: matches!(
                        &request.options,
                        AlignmentSummaryGpuLaneOptions::Sashimi(options)
                            if options.projection.show_labels
                                && options.projection.label_mode != SashimiLabelMode::None
                    ),
                    overlays: request.overlays,
                    background_range: style.background_range,
                    foreground_range: style.foreground_range,
                });
            }
            let start = u32::try_from(overlays.len())
                .expect("overlay address space was validated before allocation");
            append_summary_overlays(request, &mut overlays);
            let end = u32::try_from(overlays.len())
                .expect("overlay address space was validated before allocation");
            if start != end {
                pending_overlay_draws.push((
                    request.camera,
                    request.screen_size_points,
                    request.scissor,
                    start..end,
                ));
            }
        }
        debug_assert_eq!(pending_draws.len(), mandatory_pages);
        debug_assert_eq!(overlays.len(), overlay_count);

        let mut uniforms = vec![0_u8; uniform_bytes];
        let mut draws = Vec::with_capacity(mandatory_pages);
        let mut prepared_overlays = Vec::with_capacity(overlay_draws);
        let next_revision = self.revision.saturating_add(1);
        let mut page_leases =
            BTreeMap::<AlignmentSummaryGpuPageKey, Arc<AlignmentSummaryGpuResidentPageLease>>::new(
            );
        let mut page_pin_acquisitions = 0_usize;
        let mut pins = Vec::with_capacity(4);
        let mut uniform_index = 0_usize;
        for request in &pending_draws {
            let uniform = summary_uniform(
                request.camera,
                request.screen_size,
                request.page_start,
                request.covered_clip_x,
                request.scale_maximum,
                request.minimum_alternate_fraction,
                request.transform,
                request.quality_weighted,
                request.allele_interpretation,
                request.page.family(),
                request.hidden_modification_mask,
                request.modification_color_mode,
                request.junction_scale_maximum,
                request.junction_normalization,
                request.junction_row_count,
                request.minimum_junction_depth,
                request.minimum_junction_flank,
                request.junction_display_mode,
                request.show_junction_labels,
                request.overlays,
            );
            let offset = uniform_index.saturating_mul(self.uniform_stride);
            uniforms[offset..offset + size_of::<AlignmentSummaryGpuUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
            let uniform_offset = u32::try_from(offset)
                .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?;
            let page_index = self
                .page_index(request.page)
                .expect("mandatory summary residency was proven before frame mutation");
            self.touch_page_at(page_index, next_revision);
            let page = &self.cache.pages[page_index];
            if let Some(allocation) = &page.allocation
                && !page_leases.contains_key(&request.page)
            {
                let (lease, acquired) =
                    content_authority.leases.acquire(request.page, allocation)?;
                debug_assert_eq!(lease.key, request.page);
                page_pin_acquisitions = page_pin_acquisitions.saturating_add(usize::from(acquired));
                page_leases.insert(request.page, lease);
            }
            draws.push(PreparedSummaryDraw {
                page: request.page,
                coherence_scope: request.coherence_scope,
                lane_identity: request.lane_identity,
                camera: request.camera,
                page_start: request.page_start,
                junction_scale_maximum: request.junction_scale_maximum,
                junction_normalization: request.junction_normalization,
                junction_row_count: request.junction_row_count,
                minimum_junction_depth: request.minimum_junction_depth,
                minimum_junction_flank: request.minimum_junction_flank,
                junction_display_mode: request.junction_display_mode,
                junction_count_mode: request.junction_count_mode,
                junction_label_mode: request.junction_label_mode,
                show_junction_labels: request.show_junction_labels,
                uniform_offset,
                scissor: request.scissor,
                background_range: request.background_range.clone(),
                foreground_range: request.foreground_range.clone(),
            });
            uniform_index = uniform_index.saturating_add(1);
        }
        for (camera, screen_size, scissor, range) in pending_overlay_draws {
            let uniform = summary_uniform(
                camera,
                screen_size,
                0,
                [
                    camera.clip().x as f32,
                    (camera.clip().x + camera.clip().width) as f32,
                ],
                1,
                0.0,
                CoverageScaleTransform::Linear,
                false,
                CoverageAlleleInterpretation::GenericReferenceAware,
                SummaryPageFamily::Depth,
                0,
                ModifiedBaseColorMode::ByType,
                1.0,
                1.0,
                1,
                0,
                0,
                JunctionDisplayMode::Collapsed,
                false,
                AlignmentSummaryFrameOverlays::default(),
            );
            let offset = uniform_index.saturating_mul(self.uniform_stride);
            uniforms[offset..offset + size_of::<AlignmentSummaryGpuUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
            prepared_overlays.push(PreparedSummaryOverlayDraw {
                uniform_offset: u32::try_from(offset)
                    .map_err(|_| AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace)?,
                scissor,
                range,
            });
            uniform_index = uniform_index.saturating_add(1);
        }
        debug_assert_eq!(uniform_index, uniform_count);
        if let Some(allocation) = &frame_buffers.uniform_allocation {
            pins.push(allocation.pin()?);
        }
        if let Some(allocation) = &frame_buffers.overlay_allocation {
            pins.push(allocation.pin()?);
        }
        if let Some(allocation) = &self.cache.page_table_allocation {
            pins.push(allocation.pin()?);
        }
        if !uniforms.is_empty() {
            queue.write_buffer(
                frame_buffers
                    .uniform_buffer
                    .as_ref()
                    .expect("non-empty uniforms require a uniform buffer"),
                0,
                &uniforms,
            );
        }
        if !overlays.is_empty() {
            queue.write_buffer(
                frame_buffers
                    .overlay_buffer
                    .as_ref()
                    .expect("non-empty overlays require an overlay buffer"),
                0,
                bytemuck::cast_slice(&overlays),
            );
        }

        self.revision = next_revision;
        let retained_frame_bytes = alignment_summary_gpu_frame_bytes(
            scene_revisions.len(),
            prepared_page_count,
            camera_sequences.len(),
            resolved_depth_scales.len(),
            draws.len(),
            prepared_overlays.len(),
            pins.len()
                .saturating_add(page_leases.len())
                .saturating_add(1),
        );
        if retained_frame_bytes > frame_buffers.frame_capacity {
            return Err(AlignmentSummaryGpuPrepareError::FrameSlotCapacity {
                required: retained_frame_bytes,
                available: frame_buffers.frame_capacity,
            });
        }
        pins.push(frame_buffers.frame_allocation.pin()?);
        let frame = AlignmentSummaryGpuFrame {
            revision: next_revision,
            device_generation: self.device_generation,
            scene_revisions: scene_revisions.into(),
            camera_sequences: camera_sequences.into(),
            resolved_depth_scales: resolved_depth_scales.into(),
            content_authority: content_authority.clone(),
            draws: draws.into(),
            overlay_draws: prepared_overlays.into(),
            buffers: frame_buffers,
            _page_leases: page_leases.into_values().collect(),
            _pins: pins,
        };
        let resident_gpu_bytes = self.resident_gpu_bytes()?;
        let stats = AlignmentSummaryGpuPrepareStats {
            scenes: requests.len(),
            mandatory_pages,
            depth_instances,
            allele_instances,
            junction_instances,
            modification_instances,
            overlay_instances: overlays.len(),
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes: std::mem::take(&mut self.pending_geometry_uploaded_bytes),
            range_maximum_queries,
            junction_entries_examined,
            page_pin_acquisitions,
            page_cache_hits,
            page_cache_misses: 0,
            resident_pages: self.cache.pages.len(),
            resident_gpu_bytes,
        };
        Ok((frame, stats))
    }

    /// Captures a new camera/options/overlay presentation over unchanged resident exact-summary
    /// content. This path writes only the reusable uniform/overlay ring used by [`Self::prepare`].
    /// It never admits or retires a semantic page, rebuilds the page table, opens a replacement
    /// bundle, or uploads immutable geometry.
    pub fn capture_camera_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        authority: &AlignmentSummaryGpuContentAuthority,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
    ) -> Result<
        (AlignmentSummaryGpuFrame, AlignmentSummaryGpuPrepareStats),
        AlignmentSummaryGpuCameraCaptureError,
    > {
        if authority.device_generation != self.device_generation {
            return Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::DeviceGeneration {
                        authority: authority.device_generation,
                        renderer: self.device_generation,
                    },
                },
            );
        }

        for request in requests {
            validate_summary_request(request)?;
        }
        let requested_scene_revisions = requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        if requested_scene_revisions.as_slice() != authority.scene_revisions.as_ref() {
            return Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::SceneContent {
                        authority_scenes: authority.scene_revisions.len(),
                        requested_scenes: requested_scene_revisions.len(),
                    },
                },
            );
        }

        let requested_scene_allocations = requests
            .iter()
            .map(|request| request.scene.metadata_allocation().id())
            .collect::<Vec<_>>();
        if requested_scene_allocations.as_slice() != authority.scene_allocations.as_ref() {
            return Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::SceneContent {
                        authority_scenes: authority.scene_allocations.len(),
                        requested_scenes: requested_scene_allocations.len(),
                    },
                },
            );
        }

        let requested_selectors = summary_content_selectors(requests);
        if requested_selectors.as_slice() != authority.content_selectors.as_ref() {
            return Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::ContentContract {
                        authority_lanes: authority.content_selectors.len(),
                        requested_lanes: requested_selectors.len(),
                    },
                },
            );
        }

        let mandatory = Self::mandatory_page_keys(requests)?;
        let mut missing = mandatory
            .iter()
            .copied()
            .filter(|key| self.page_index(*key).is_none());
        if let Some(first_missing) = missing.next() {
            let missing_pages = 1_usize.saturating_add(missing.count());
            return Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::MissingResidentPages {
                        missing: missing_pages,
                        first_missing,
                    },
                },
            );
        }

        // Retain exact transient scratch accounting while selecting only a precharged retained
        // frame slot. Transient work does not participate in replacement-bundle single flight.
        let result =
            self.capture_resident_camera_frame(device, queue, requests, ledger, authority, false);
        let (mut frame, stats) = match result {
            Ok(prepared) => prepared,
            Err(AlignmentSummaryGpuPrepareError::FrameSlotBusy { slots }) => {
                return Err(AlignmentSummaryGpuCameraCaptureError::FrameSlotBusy { slots });
            }
            Err(AlignmentSummaryGpuPrepareError::FrameSlotCapacity {
                required,
                available,
            }) => {
                return Err(
                    AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                        reason: AlignmentSummaryGpuContentReplacementReason::FrameEnvelope {
                            required,
                            available,
                        },
                    },
                );
            }
            Err(error) => return Err(error.into()),
        };
        debug_assert_eq!(&frame.content_authority, authority);
        frame.content_authority = authority.clone();
        Ok((frame, stats))
    }

    /// Returns the complete sorted page-key set needed by the current foreground cameras. Shared
    /// cache coordination uses this inventory to protect every upcoming mandatory summary lane
    /// while either renderer releases speculative pages.
    pub fn mandatory_page_keys(
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    ) -> Result<Vec<AlignmentSummaryGpuPageKey>, AlignmentSummaryGpuPageError> {
        Ok(unique_summary_requests(requests, true)?
            .into_iter()
            .map(|request| request.key)
            .collect())
    }

    /// Returns the stable full prepared-scene key set used as speculative-generation authority.
    /// Camera-only movement inside the same prepared horizon leaves this set unchanged.
    pub fn prepared_page_keys(
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    ) -> Result<Vec<AlignmentSummaryGpuPageKey>, AlignmentSummaryGpuPageError> {
        Ok(unique_summary_requests(requests, false)?
            .into_iter()
            .map(|request| request.key)
            .collect())
    }

    /// Atomically admits the complete visible depth/allele dependency set, then captures a
    /// frame. A camera-only repeat retains all page buffers and uploads uniforms/overlays only.
    pub fn prepare_with_admission(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<
        (
            AlignmentSummaryGpuFrame,
            AlignmentSummaryGpuPrepareStats,
            AlignmentSummaryGpuBatchAdmission,
        ),
        AlignmentSummaryGpuFrameBuildError,
    > {
        let owned = unique_summary_requests(requests, true)?;
        let borrowed = borrowed_summary_requests(&owned);
        let admission =
            self.admit_mandatory_pages(device, queue, &borrowed, ledger, publication_revision)?;
        let (frame, stats) = self.prepare(device, queue, requests, ledger)?;
        Ok((frame, stats, admission))
    }

    pub fn prefetch_prepared_scene_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentSummaryGpuBatchAdmission, AlignmentSummaryGpuPageAdmissionError> {
        let owned = unique_summary_requests(requests, false)?;
        let borrowed = borrowed_summary_requests(&owned);
        let admission = self.admit_pages(device, queue, &borrowed, ledger, publication_revision)?;
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_sub(admission.uploaded_bytes);
        Ok(admission)
    }

    pub fn prefetch_prepared_scene_pages_bounded(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
        maximum_pages: NonZeroUsize,
    ) -> Result<AlignmentSummaryGpuPrefetchProgress, AlignmentSummaryGpuPageAdmissionError> {
        let pages = unique_summary_requests(requests, false)?;
        let prepared_pages = pages.len();
        let missing_pages_before = pages
            .iter()
            .filter(|page| self.page_index(page.key).is_none())
            .count();
        let batch = pages
            .iter()
            .filter(|page| self.page_index(page.key).is_none())
            .take(maximum_pages.get())
            .cloned()
            .collect::<Vec<_>>();
        let borrowed_batch = borrowed_summary_requests(&batch);
        let protected = borrowed_summary_requests(&pages);
        let admission = self.admit_pages_protecting(
            device,
            queue,
            &borrowed_batch,
            &protected,
            ledger,
            publication_revision,
            ReservationKind::Normal,
        )?;
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_sub(admission.uploaded_bytes);
        let remaining_missing_pages = pages
            .iter()
            .filter(|page| self.page_index(page.key).is_none())
            .count();
        Ok(AlignmentSummaryGpuPrefetchProgress {
            prepared_pages,
            missing_pages_before,
            admission,
            remaining_missing_pages,
            missing_pages_remain: remaining_missing_pages > 0,
            retained_bytes: 0,
            retention_limited: false,
        })
    }

    /// Starts a summary warm-up in O(scene lanes). Each later advance resolves only its bounded
    /// slice of the replacement-minted authority.
    pub fn begin_prepared_scene_prefetch_cursor(
        &self,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        page_keys: Arc<[AlignmentSummaryGpuPageKey]>,
        maximum_total_pinned_bytes: usize,
    ) -> Result<AlignmentSummaryGpuPrefetchCursor, AlignmentSummaryGpuPrefetchCursorError> {
        let selectors = summary_content_selectors(requests);
        let scenes = requests
            .iter()
            .zip(selectors)
            .map(|(request, selector)| {
                (
                    Arc::clone(request.scene),
                    selector,
                    request.junction_presentation.clone(),
                )
            })
            .collect();
        Ok(AlignmentSummaryGpuPrefetchCursor {
            device_generation: self.device_generation,
            page_keys,
            scenes,
            next_page: 0,
            pins: Vec::new(),
            retained_bytes: 0,
            maximum_total_pinned_bytes,
            retention_limited: false,
        })
    }

    pub fn advance_prepared_scene_prefetch_cursor(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cursor: &mut AlignmentSummaryGpuPrefetchCursor,
        ledger: &AllocationLedger,
        publication_revision: u64,
        maximum_pages: NonZeroUsize,
    ) -> Result<AlignmentSummaryGpuPrefetchProgress, AlignmentSummaryGpuPrefetchCursorError> {
        if cursor.device_generation != self.device_generation {
            return Err(AlignmentSummaryGpuPrefetchCursorError::DeviceGeneration {
                cursor: cursor.device_generation,
                renderer: self.device_generation,
            });
        }
        let authority_pages_before = cursor.remaining_pages();
        let end = cursor
            .next_page
            .saturating_add(maximum_pages.get())
            .min(cursor.page_keys.len());
        let inspected = end.saturating_sub(cursor.next_page);
        self.prefetch_authority_keys_inspected
            .fetch_add(inspected as u64, Ordering::Relaxed);
        let mut batch = Vec::with_capacity(inspected);
        let mut resident_pins = Vec::new();
        let mut resident_retained_bytes = 0_usize;
        for key in cursor.page_keys[cursor.next_page..end].iter().copied() {
            let page = summary_cursor_page(&cursor.scenes, key)?;
            if let Some(resident) = self.page(key) {
                if let Some(allocation) = &resident.allocation {
                    let capacity = allocation.snapshot()?.charged_capacity;
                    if let Some(pin) =
                        allocation.pin_if_total_at_most(cursor.maximum_total_pinned_bytes)?
                    {
                        resident_pins.push(pin);
                        resident_retained_bytes = resident_retained_bytes.saturating_add(capacity);
                    } else {
                        cursor.retention_limited = true;
                    }
                }
            } else {
                batch.push(page);
            }
        }
        let borrowed = borrowed_summary_requests(&batch);
        let admission = self.admit_pages(device, queue, &borrowed, ledger, publication_revision)?;
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_sub(admission.uploaded_bytes);
        for page in &batch {
            let resident = self
                .page(page.key)
                .expect("successful summary cursor admission makes every batch page resident");
            if let Some(allocation) = &resident.allocation {
                let capacity = allocation.snapshot()?.charged_capacity;
                if let Some(pin) =
                    allocation.pin_if_total_at_most(cursor.maximum_total_pinned_bytes)?
                {
                    cursor.pins.push(pin);
                    cursor.retained_bytes = cursor.retained_bytes.saturating_add(capacity);
                } else {
                    cursor.retention_limited = true;
                }
            }
        }
        cursor.pins.extend(resident_pins);
        cursor.retained_bytes = cursor
            .retained_bytes
            .saturating_add(resident_retained_bytes);
        cursor.next_page = end;
        let remaining_authority_pages = cursor.remaining_pages();
        Ok(AlignmentSummaryGpuPrefetchProgress {
            prepared_pages: cursor.page_keys.len(),
            missing_pages_before: authority_pages_before,
            admission,
            remaining_missing_pages: remaining_authority_pages,
            missing_pages_remain: remaining_authority_pages > 0,
            retained_bytes: cursor.retained_bytes,
            retention_limited: cursor.retention_limited,
        })
    }

    pub fn finish_prepared_scene_prefetch_cursor(
        &self,
        cursor: AlignmentSummaryGpuPrefetchCursor,
    ) -> Result<AlignmentSummaryGpuResidencyLease, AlignmentSummaryGpuPrefetchCursorError> {
        if cursor.device_generation != self.device_generation {
            return Err(AlignmentSummaryGpuPrefetchCursorError::DeviceGeneration {
                cursor: cursor.device_generation,
                renderer: self.device_generation,
            });
        }
        if !cursor.is_complete() {
            return Err(AlignmentSummaryGpuPrefetchCursorError::Incomplete {
                remaining: cursor.remaining_pages(),
            });
        }
        if cursor.retention_limited {
            return Err(
                AlignmentSummaryGpuPrefetchCursorError::ResidencyBudgetLimited {
                    retained_bytes: cursor.retained_bytes,
                    maximum_total_pinned_bytes: cursor.maximum_total_pinned_bytes,
                },
            );
        }
        Ok(AlignmentSummaryGpuResidencyLease {
            device_generation: cursor.device_generation,
            page_keys: cursor.page_keys,
            _pins: cursor.pins,
        })
    }

    pub fn pin_prepared_scene_pages(
        &self,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    ) -> Result<AlignmentSummaryGpuResidencyLease, AlignmentSummaryGpuResidencyError> {
        let pages = unique_summary_requests(requests, false)?;
        let missing = pages
            .iter()
            .filter(|page| self.page_index(page.key).is_none())
            .map(|page| page.key)
            .collect::<Vec<_>>();
        if let Some(first_missing) = missing.first().copied() {
            return Err(AlignmentSummaryGpuResidencyError::MissingPreparedPages {
                missing: missing.len(),
                first_missing,
            });
        }
        let mut page_keys = Vec::with_capacity(pages.len());
        let mut pins = Vec::with_capacity(pages.len());
        for page in pages {
            let resident = self
                .page(page.key)
                .expect("complete prepared-page residency was proven before pinning");
            page_keys.push(page.key);
            if let Some(allocation) = &resident.allocation {
                pins.push(allocation.pin()?);
            }
        }
        Ok(AlignmentSummaryGpuResidencyLease {
            device_generation: self.device_generation,
            page_keys: page_keys.into(),
            _pins: pins,
        })
    }

    pub fn render<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass AlignmentSummaryGpuFrame,
    ) -> Result<(), AlignmentSummaryGpuRenderError> {
        self.render_within(pass, frame, [u32::MAX; 2])
    }

    /// Replays `frame` like [`Self::render`], but first intersects every captured scissor with a
    /// render target of `target_size` physical pixels. A frame is captured against one target
    /// size and may be replayed after a resize or into a differently sized capture texture; a
    /// scissor outside the attachment is a wgpu validation error, so draws whose scissor falls
    /// wholly outside the target are skipped and the rest are clipped.
    // Same error type as `render`, which this supersedes.
    #[allow(clippy::result_large_err)]
    pub fn render_within<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass AlignmentSummaryGpuFrame,
        target_size: [u32; 2],
    ) -> Result<(), AlignmentSummaryGpuRenderError> {
        if frame.device_generation != self.device_generation {
            return Err(AlignmentSummaryGpuRenderError::DeviceGenerationMismatch {
                frame: frame.device_generation,
                renderer: self.device_generation,
            });
        }
        if frame.draws.is_empty() && frame.overlay_draws.is_empty() {
            return Ok(());
        }
        let bind_group = frame
            .buffers
            .uniform_bind_group
            .as_ref()
            .ok_or(AlignmentSummaryGpuRenderError::MissingUniformBuffer)?;
        let mut palette_bound = false;
        for draw in frame.draws.iter() {
            let page = self
                .page(draw.page)
                .ok_or(AlignmentSummaryGpuRenderError::MissingPinnedPage(draw.page))?;
            pass.set_pipeline(match draw.page.family() {
                SummaryPageFamily::Depth => &self.depth_pipeline,
                SummaryPageFamily::Alleles => &self.allele_pipeline,
                SummaryPageFamily::Modifications => &self.modification_pipeline,
                SummaryPageFamily::Junctions => &self.junction_pipeline,
                family => {
                    return Err(AlignmentSummaryGpuRenderError::UnsupportedFamily(family));
                }
            });
            let Some([x, y, width, height]) = crate::clamp_scissor_to_target(
                [
                    draw.scissor.x,
                    draw.scissor.y,
                    draw.scissor.width,
                    draw.scissor.height,
                ],
                target_size,
            ) else {
                continue;
            };
            pass.set_scissor_rect(x, y, width, height);
            pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
            if !palette_bound {
                pass.set_bind_group(1, self.palette.bind_group(), &[]);
                palette_bound = true;
            }
            if let Some(buffer) = &page.buffer
                && page.instance_count > 0
            {
                pass.set_vertex_buffer(0, buffer.slice(..));
                if !draw.background_range.is_empty() {
                    pass.draw(0..6, draw.background_range.clone());
                }
                if !draw.foreground_range.is_empty() {
                    pass.draw(0..6, draw.foreground_range.clone());
                }
            }
        }
        if !frame.overlay_draws.is_empty() {
            let overlay = frame
                .buffers
                .overlay_buffer
                .as_ref()
                .ok_or(AlignmentSummaryGpuRenderError::MissingOverlayBuffer)?;
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(1, self.palette.bind_group(), &[]);
            pass.set_vertex_buffer(0, overlay.slice(..));
            for draw in frame.overlay_draws.iter() {
                let Some([x, y, width, height]) = crate::clamp_scissor_to_target(
                    [
                        draw.scissor.x,
                        draw.scissor.y,
                        draw.scissor.width,
                        draw.scissor.height,
                    ],
                    target_size,
                ) else {
                    continue;
                };
                pass.set_scissor_rect(x, y, width, height);
                pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
                pass.draw(0..6, draw.range.clone());
            }
        }
        Ok(())
    }

    /// Hit-tests the exact procedural junction geometry carried by a rendered frame. The same
    /// immutable metadata and projection oracle drive pixels and picking; no query-local junction
    /// index or legacy display rectangle participates. Hidden (weak or filtered) junctions are
    /// not drawn and so cannot be hit.
    #[must_use]
    pub fn hit_test_junction_lane(
        &self,
        frame: &AlignmentSummaryGpuFrame,
        coherence_scope: u64,
        lane_identity: u64,
        x: f64,
        y: f64,
        maximum_distance: f64,
    ) -> Option<AlignmentSummaryGpuJunctionHit> {
        if frame.device_generation != self.device_generation
            || !x.is_finite()
            || !y.is_finite()
            || !maximum_distance.is_finite()
            || maximum_distance < 0.0
        {
            return None;
        }
        let mut best = None::<AlignmentSummaryGpuJunctionHit>;
        for draw in frame.draws.iter().rev().filter(|draw| {
            draw.page.family() == SummaryPageFamily::Junctions
                && draw.coherence_scope == coherence_scope
                && draw.lane_identity == lane_identity
        }) {
            let clip = draw.camera.clip();
            if x < clip.x - maximum_distance
                || x > clip.x + clip.width + maximum_distance
                || y < clip.y - maximum_distance
                || y > clip.y + clip.height + maximum_distance
            {
                continue;
            }
            let page = self.page(draw.page)?;
            let GpuSummaryPageMetadata::Junctions(metadata) = &page.metadata else {
                continue;
            };
            for entry in metadata.entries().iter().copied() {
                let Some((projected, component)) = project_drawn_junction(draw, entry) else {
                    continue;
                };
                let Some(distance) = junction_arc_distance(
                    projected.bounds,
                    projected.stroke_width,
                    component == AlignmentSummaryJunctionComponentToken::Negative,
                    x,
                    y,
                ) else {
                    continue;
                };
                let acquisition = maximum_distance.max(projected.stroke_width * 0.5 + 2.0);
                if distance > acquisition
                    || best.is_some_and(|current| current.distance <= distance)
                {
                    continue;
                }
                best = Some(AlignmentSummaryGpuJunctionHit {
                    coherence_scope: draw.coherence_scope,
                    lane_identity: draw.lane_identity,
                    evidence: entry.evidence(),
                    annotation: entry.annotation(),
                    component,
                    bounds: projected.bounds,
                    distance,
                    presentation: entry.presentation(),
                    stroke_width: projected.stroke_width,
                });
            }
        }
        best
    }

    /// Projects a bounded set of labels from the same pinned junction pages used for pixels and
    /// picking. This is presentation-only work: it neither queries the alignment source nor
    /// rebuilds/uploads persistent geometry.
    ///
    /// Each drawn junction offers one label at its arc's apex (on the part of the arc that is on
    /// screen). Where labels would overlap, the junction with more reads keeps its label.
    #[must_use]
    pub fn junction_labels(
        &self,
        frame: &AlignmentSummaryGpuFrame,
    ) -> Vec<AlignmentSummaryGpuJunctionLabel> {
        if frame.device_generation != self.device_generation {
            return Vec::new();
        }
        let mut candidates =
            BTreeMap::<(u64, u64), Vec<(AlignmentSummaryGpuJunctionLabel, u64)>>::new();
        let mut seen = BTreeSet::<(u64, u64, u64, u64)>::new();
        for draw in frame.draws.iter().filter(|draw| {
            draw.page.family() == SummaryPageFamily::Junctions && draw.show_junction_labels
        }) {
            let lane = (draw.coherence_scope, draw.lane_identity);
            let Some(page) = self.page(draw.page) else {
                continue;
            };
            let GpuSummaryPageMetadata::Junctions(metadata) = &page.metadata else {
                continue;
            };
            for entry in metadata.entries().iter().copied() {
                let evidence = entry.evidence();
                let Some((projected, component)) = project_drawn_junction(draw, entry) else {
                    continue;
                };
                // A junction drawn by two pages of one lane (never expected) labels once.
                if !seen.insert((lane.0, lane.1, evidence.start, evidence.end)) {
                    continue;
                }
                let row_count = match draw.junction_display_mode {
                    JunctionDisplayMode::Collapsed => 1,
                    JunctionDisplayMode::Expanded => draw.junction_row_count.max(1),
                };
                let band_height = draw.camera.clip().height / f64::from(row_count);
                // Arc bounds are unclamped; a label is anchored to the part of the arc that is
                // on screen and follows the curve there, so a junction whose apex is off-screen
                // still gets a visible label next to its visible stroke.
                let clip = draw.camera.clip();
                let visible_left = projected.bounds.x.max(clip.x);
                let visible_right =
                    (projected.bounds.x + projected.bounds.width).min(clip.x + clip.width);
                let visible_width = visible_right - visible_left;
                if visible_width < 18.0 || band_height < 28.0 {
                    continue;
                }
                let band_top = clip.y + f64::from(projected.expanded_row) * band_height;
                let band_bottom = band_top + band_height;
                let x = (visible_left + visible_right) * 0.5;
                let curve_offset = junction_arc_curve_offset(
                    (x - projected.bounds.x) / projected.bounds.width,
                    projected.bounds.height,
                    projected.stroke_width,
                );
                let downward = component == AlignmentSummaryJunctionComponentToken::Negative;
                let centre = if downward {
                    projected.bounds.y + projected.bounds.height - curve_offset
                } else {
                    projected.bounds.y + curve_offset
                };
                // Just beyond the stroke, on the outside of the arc.
                let offset = projected.stroke_width * 0.5 + JUNCTION_LABEL_HALF_HEIGHT + 0.5;
                let y = (if downward {
                    centre + offset
                } else {
                    centre - offset
                })
                .clamp(
                    band_top + JUNCTION_LABEL_HALF_HEIGHT,
                    band_bottom - JUNCTION_LABEL_HALF_HEIGHT,
                );
                let text = match draw.junction_label_mode {
                    SashimiLabelMode::Psi => {
                        rgv_model::psi_label(&entry.presentation(), evidence.depth)
                    }
                    SashimiLabelMode::Counts | SashimiLabelMode::None => junction_count_label(
                        evidence,
                        draw.junction_count_mode,
                        f64::from(draw.junction_normalization),
                    ),
                };
                let label_width = junction_label_width(&text);
                let label_box = [
                    x - label_width * 0.5,
                    y - JUNCTION_LABEL_HALF_HEIGHT,
                    x + label_width * 0.5,
                    y + JUNCTION_LABEL_HALF_HEIGHT,
                ];
                candidates.entry(lane).or_default().push((
                    AlignmentSummaryGpuJunctionLabel {
                        coherence_scope: draw.coherence_scope,
                        lane_identity: draw.lane_identity,
                        evidence,
                        x,
                        y,
                        text,
                        chip: label_box,
                    },
                    u64::from(evidence.depth),
                ));
            }
        }
        let mut labels = Vec::new();
        for (_, lane_candidates) in candidates {
            let boxes = lane_candidates
                .iter()
                .map(|(label, depth)| (label.chip, *depth))
                .collect::<Vec<_>>();
            let kept = rgv_model::place_labels_by_priority(&boxes, 1.0);
            let mut lane_labels = lane_candidates;
            let mut index = 0_usize;
            lane_labels.retain(|_| {
                let keep = kept.binary_search(&index).is_ok();
                index += 1;
                keep
            });
            // Bound the text per lane, keeping the strongest junctions' labels.
            if lane_labels.len() > MAX_RENDERED_JUNCTION_LABELS {
                lane_labels.sort_by_key(|(label, depth)| {
                    (
                        std::cmp::Reverse(*depth),
                        label.evidence.start,
                        label.evidence.end,
                    )
                });
                lane_labels.truncate(MAX_RENDERED_JUNCTION_LABELS);
                lane_labels.sort_by_key(|(label, _)| (label.evidence.start, label.evidence.end));
            }
            labels.extend(lane_labels.into_iter().map(|(label, _)| label));
        }
        labels
    }

    /// Rebuilds this renderer for a replacement GPU device, for example after the previous device
    /// was lost. Every pipeline is recreated against `device` and `target_format`, every
    /// device-derived page and frame buffer is dropped (releasing its ledger charge), and the
    /// device generation advances so frames, leases and plans captured against the old device
    /// are rejected instead of being replayed. Semantic scenes are untouched and are simply
    /// admitted again.
    pub fn rebuild_for_device(
        &mut self,
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) {
        self.discard_device_resources();
        let mut rebuilt = Self::with_palette(device, target_format, self.palette.palette());
        rebuilt.device_generation = self.device_generation;
        rebuilt.revision = self.revision;
        for (rebuilt, previous) in [
            (
                &rebuilt.content_manifest_builds,
                &self.content_manifest_builds,
            ),
            (
                &rebuilt.content_manifest_pages_enumerated,
                &self.content_manifest_pages_enumerated,
            ),
            (
                &rebuilt.prefetch_authority_keys_inspected,
                &self.prefetch_authority_keys_inspected,
            ),
            (
                &rebuilt.reclaim_authority_keys_inspected,
                &self.reclaim_authority_keys_inspected,
            ),
        ] {
            rebuilt.store(previous.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        *self = rebuilt;
    }

    /// Drops every device-derived allocation while preserving all scene-owned semantic pages.
    pub fn discard_device_resources(&mut self) {
        self.cache.clear();
        self.frame_buffers.clear();
        self.pending_geometry_uploaded_bytes = 0;
        self.device_generation = self.device_generation.saturating_add(1).max(1);
    }

    #[must_use]
    pub fn resident_page_count(&self) -> usize {
        self.cache.pages.len()
    }

    /// Cooperatively releases renderer-local retained capacity until the exact failed shared-
    /// ledger request fits its original limit or no safe candidate remains. This preserves
    /// transition-reserve semantics when the current charged total is legitimately above the
    /// base budget. Idle frame-ring slots are dropped first, followed by an unpinned sparse page
    /// table and then unpinned pages from the online deterministic
    /// `(last_used_revision, page_key)` index. Planning stops once the exact goal is covered; it
    /// never rebuilds or sorts a full failure-time page inventory.
    /// `protected_pages` are never evicted by this pass; active frames and residency leases are
    /// independently protected by ledger pins.
    pub fn reclaim_for_goal(
        &mut self,
        ledger: &AllocationLedger,
        goal: GpuReclaimGoal,
        protected_pages: &[AlignmentSummaryGpuPageKey],
    ) -> Result<GpuReclaimReport, LedgerError> {
        paged_cache::reclaim_for_goal(
            &mut self.cache,
            &mut self.frame_buffers,
            ledger,
            goal,
            protected_pages,
            self.revision,
            &self.reclaim_authority_keys_inspected,
        )
    }

    /// Creates a reclaim goal from the post-unwind ledger and applies it to this renderer. Shared
    /// coordinators should create one goal and pass it unchanged to every participant via
    /// [`Self::reclaim_for_goal`].
    pub fn reclaim_for_budget_pressure(
        &mut self,
        ledger: &AllocationLedger,
        pressure: GpuBudgetPressure,
        protected_pages: &[AlignmentSummaryGpuPageKey],
    ) -> Result<GpuReclaimReport, LedgerError> {
        let snapshot = ledger.progress()?;
        let goal =
            GpuReclaimGoal::new(pressure, snapshot.charged.saturating_add(snapshot.reserved));
        self.reclaim_for_goal(ledger, goal, protected_pages)
    }

    /// Convenience wrapper for callers whose failed request used the ledger's base retained
    /// budget rather than transition reserve.
    pub fn reclaim_for_headroom(
        &mut self,
        ledger: &AllocationLedger,
        required_headroom_bytes: usize,
        protected_pages: &[AlignmentSummaryGpuPageKey],
    ) -> Result<GpuReclaimReport, LedgerError> {
        let snapshot = ledger.progress()?;
        let pressure = GpuBudgetPressure::from_budget_failure(
            required_headroom_bytes,
            snapshot.charged,
            snapshot.reserved,
            snapshot.budget,
        );
        self.reclaim_for_budget_pressure(ledger, pressure, protected_pages)
    }

    #[must_use]
    pub fn contains_page(&self, key: AlignmentSummaryGpuPageKey) -> bool {
        self.page_index(key).is_some()
    }

    fn resident_retained_bytes(&self) -> Result<usize, LedgerError> {
        paged_cache::resident_retained_bytes(&self.cache, &self.frame_buffers)
    }

    fn resident_gpu_bytes(&self) -> Result<usize, LedgerError> {
        paged_cache::resident_gpu_bytes(&self.cache, &self.frame_buffers)
    }

    fn page_index(&self, key: AlignmentSummaryGpuPageKey) -> Option<usize> {
        self.cache.page_index(key)
    }

    fn page(&self, key: AlignmentSummaryGpuPageKey) -> Option<&GpuSummaryPage> {
        self.cache.page(key)
    }

    fn page_mut(&mut self, key: AlignmentSummaryGpuPageKey) -> Option<&mut GpuSummaryPage> {
        self.cache.page_mut(key)
    }

    fn rebuild_reclaim_index(&mut self) {
        self.cache.rebuild_reclaim_index();
    }

    fn touch_page_at(&mut self, index: usize, revision: u64) {
        self.cache.touch_page_at(index, revision);
    }

    fn ensure_page_table_capacity(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        reservation_kind: ReservationKind,
    ) -> Result<(), AlignmentSummaryGpuPageAdmissionError> {
        Ok(self.cache.ensure_page_table_capacity(
            ledger,
            required,
            reservation_kind,
            self.revision,
        )?)
    }

    fn acquire_frame_buffers(
        &mut self,
        device: &wgpu::Device,
        ledger: &AllocationLedger,
        uniforms: usize,
        overlays: usize,
        frame_bytes: usize,
    ) -> Result<Arc<GpuSummaryFrameBuffers>, AlignmentSummaryGpuPrepareError> {
        if let Some(index) = self
            .frame_buffers
            .iter()
            .enumerate()
            .filter(|(_, slot)| Arc::strong_count(slot) == 1)
            .filter(|(_, slot)| {
                slot.uniform_capacity >= uniforms
                    && slot.overlay_capacity >= overlays
                    && slot.frame_capacity >= frame_bytes
            })
            .min_by_key(|(_, slot)| (slot.uniform_capacity, slot.overlay_capacity))
            .map(|(index, _)| index)
        {
            let slot = Arc::clone(&self.frame_buffers[index]);
            // A previous overlap of in-flight frames can leave several now-idle ring slots. Once
            // one is selected, retain only it and genuinely in-flight slots so the cache reaches a
            // stable high-water mark instead of keeping every historical capacity class forever.
            self.frame_buffers.retain(|candidate| {
                Arc::strong_count(candidate) > 1 || Arc::ptr_eq(candidate, &slot)
            });
            return Ok(slot);
        }
        // No idle slot can serve the frame. Reclaim all idle capacities before reserving the new
        // one; active frames retain their slots through their `Arc` and allocation pins.
        self.reclaim_all_idle_frame_buffers();
        let uniform_capacity = (uniforms > 0)
            .then(|| next_summary_capacity(uniforms))
            .transpose()?
            .unwrap_or(0);
        let overlay_capacity = (overlays > 0)
            .then(|| next_summary_capacity(overlays))
            .transpose()?
            .unwrap_or(0);
        let uniform_bytes = uniform_capacity.saturating_mul(self.uniform_stride);
        let overlay_bytes =
            overlay_capacity.saturating_mul(size_of::<AlignmentSummaryGpuOverlayInstance>());
        validate_device_buffer_size(device, uniform_bytes)?;
        validate_device_buffer_size(device, overlay_bytes)?;
        let mut uniform_reservation = (uniform_bytes > 0)
            .then(|| {
                ledger.reserve(
                    AllocationClass::GpuPage,
                    uniform_bytes,
                    ReservationKind::Transition,
                )
            })
            .transpose()?;
        let mut overlay_reservation = (overlay_bytes > 0)
            .then(|| {
                ledger.reserve(
                    AllocationClass::GpuPage,
                    overlay_bytes,
                    ReservationKind::Transition,
                )
            })
            .transpose()?;
        let mut frame_reservation = ledger.reserve(
            AllocationClass::TransitionFrame,
            frame_bytes,
            ReservationKind::Transition,
        )?;
        let uniform_candidate = (uniform_bytes > 0).then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV alignment summary frame uniform ring"),
                size: uniform_bytes as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = create_summary_bind_group(device, &self.uniform_layout, &buffer);
            (buffer, bind_group)
        });
        let overlay_candidate = (overlay_bytes > 0).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV alignment summary frame overlay ring"),
                size: overlay_bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let uniform_allocation = uniform_reservation
            .as_mut()
            .map(|reservation| reservation.register(uniform_bytes, self.revision))
            .transpose()?;
        let overlay_allocation = overlay_reservation
            .as_mut()
            .map(|reservation| reservation.register(overlay_bytes, self.revision))
            .transpose()?;
        let frame_allocation = frame_reservation.register(frame_bytes, self.revision)?;
        let (uniform_buffer, uniform_bind_group) = uniform_candidate
            .map_or((None, None), |(buffer, bind_group)| {
                (Some(buffer), Some(bind_group))
            });
        let slot = Arc::new(GpuSummaryFrameBuffers {
            uniform_buffer,
            uniform_bind_group,
            uniform_allocation,
            uniform_capacity,
            overlay_buffer: overlay_candidate,
            overlay_allocation,
            overlay_capacity,
            frame_allocation,
            frame_capacity: frame_bytes,
            capture_ring: false,
        });
        self.frame_buffers.push(Arc::clone(&slot));
        Ok(slot)
    }

    fn acquire_existing_frame_buffers(
        &self,
        uniforms: usize,
        overlays: usize,
        frame_bytes: usize,
    ) -> Result<Arc<GpuSummaryFrameBuffers>, AlignmentSummaryGpuPrepareError> {
        let mut adequate_slots = 0_usize;
        let mut available = 0_usize;
        for slot in &self.frame_buffers {
            available = available.max(slot.frame_capacity);
            if slot.uniform_capacity < uniforms
                || slot.overlay_capacity < overlays
                || slot.frame_capacity < frame_bytes
            {
                continue;
            }
            adequate_slots = adequate_slots.saturating_add(1);
            if Arc::strong_count(slot) == 1 {
                return Ok(Arc::clone(slot));
            }
        }
        if adequate_slots >= ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT {
            Err(AlignmentSummaryGpuPrepareError::FrameSlotBusy {
                slots: adequate_slots,
            })
        } else {
            Err(AlignmentSummaryGpuPrepareError::FrameSlotCapacity {
                required: frame_bytes,
                available,
            })
        }
    }

    fn reclaim_idle_frame_buffers(&mut self) {
        paged_cache::reclaim_idle_frame_slots(&mut self.frame_buffers);
    }

    fn reclaim_all_idle_frame_buffers(&mut self) {
        paged_cache::reclaim_all_idle_frame_slots(&mut self.frame_buffers);
    }

    fn evict_unpinned_pages_for_excluding(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        limit: usize,
        protected: &[AlignmentSummaryGpuPageKey],
    ) -> Result<(), LedgerError> {
        self.reclaim_idle_frame_buffers();
        self.cache.evict_unpinned_pages_for_excluding(
            ledger,
            required,
            limit,
            protected,
            self.revision,
            &self.reclaim_authority_keys_inspected,
        )
    }
}

#[derive(Clone, Debug)]
struct OwnedSummaryPageRequest {
    key: AlignmentSummaryGpuPageKey,
    page: Arc<SummarySemanticPage>,
    companion_depth: Option<Arc<SummarySemanticPage>>,
    junction_presentation: Option<Arc<SashimiPresentationTable>>,
}

impl OwnedSummaryPageRequest {
    fn borrowed(&self) -> AlignmentSummaryGpuPageRequest<'_> {
        AlignmentSummaryGpuPageRequest {
            page: &self.page,
            companion_depth: self.companion_depth.as_ref(),
            junction_presentation: self.junction_presentation.as_ref(),
        }
    }
}

fn summary_requests_for_scene(
    request: &AlignmentSummaryGpuSceneRequest<'_>,
    visible_only: bool,
) -> Result<Vec<OwnedSummaryPageRequest>, AlignmentSummaryGpuPageError> {
    let covered = if visible_only {
        summary_covered_interval(request)
    } else {
        Some(request.scene.prepared_interval())
    };
    if matches!(request.options, AlignmentSummaryGpuLaneOptions::Sashimi(_)) {
        let mut pages = covered
            .map(|interval| {
                request
                    .scene
                    .summary_pages_for(SummaryPageFamily::Junctions, interval)
            })
            .unwrap_or_default()
            .into_iter()
            .map(|page| {
                Ok(OwnedSummaryPageRequest {
                    key: AlignmentSummaryGpuPageKey::junctions_with_presentation(
                        page,
                        request.junction_presentation.as_deref(),
                    )?,
                    page: Arc::clone(page),
                    companion_depth: None,
                    junction_presentation: request.junction_presentation.clone(),
                })
            })
            .collect::<Result<Vec<_>, AlignmentSummaryGpuPageError>>()?;
        pages.sort_unstable_by_key(|page| page.key);
        pages.dedup_by_key(|page| page.key);
        return Ok(pages);
    }
    if matches!(
        request.options,
        AlignmentSummaryGpuLaneOptions::Modifications(_)
    ) {
        let mut pages = covered
            .map(|interval| {
                request
                    .scene
                    .summary_pages_for(SummaryPageFamily::Modifications, interval)
            })
            .unwrap_or_default()
            .into_iter()
            .map(|page| {
                Ok(OwnedSummaryPageRequest {
                    key: AlignmentSummaryGpuPageKey::modifications(page)?,
                    page: Arc::clone(page),
                    companion_depth: None,
                    junction_presentation: None,
                })
            })
            .collect::<Result<Vec<_>, AlignmentSummaryGpuPageError>>()?;
        pages.sort_unstable_by_key(|page| page.key);
        pages.dedup_by_key(|page| page.key);
        return Ok(pages);
    }
    let AlignmentSummaryGpuLaneOptions::Coverage(options) = &request.options else {
        unreachable!("all renderer lane variants were handled above")
    };
    let depth_pages = covered
        .map(|interval| {
            request
                .scene
                .summary_pages_for(SummaryPageFamily::Depth, interval)
        })
        .unwrap_or_default();
    let mut pages = Vec::with_capacity(depth_pages.len().saturating_mul(2));
    for page in &depth_pages {
        pages.push(OwnedSummaryPageRequest {
            key: AlignmentSummaryGpuPageKey::depth(page)?,
            page: Arc::clone(page),
            companion_depth: None,
            junction_presentation: None,
        });
    }
    if options.alleles.is_some() {
        for page in covered
            .map(|interval| {
                request
                    .scene
                    .summary_pages_for(SummaryPageFamily::Alleles, interval)
            })
            .unwrap_or_default()
        {
            let companion = request
                .scene
                .summary_pages_for(SummaryPageFamily::Depth, page.interval())
                .into_iter()
                .find(|candidate| {
                    candidate.key().tile() == page.key().tile()
                        && candidate.interval() == page.interval()
                })
                .ok_or(AlignmentSummaryGpuPageError::MissingCompanionDepth(
                    page.key(),
                ))?;
            pages.push(OwnedSummaryPageRequest {
                key: AlignmentSummaryGpuPageKey::alleles(page, companion)?,
                page: Arc::clone(page),
                companion_depth: Some(Arc::clone(companion)),
                junction_presentation: None,
            });
        }
    }
    pages.sort_unstable_by_key(|page| (summary_family_draw_order(page.key.family()), page.key));
    pages.dedup_by_key(|page| page.key);
    Ok(pages)
}

const fn summary_family_draw_order(family: SummaryPageFamily) -> u8 {
    match family {
        SummaryPageFamily::Depth => 0,
        SummaryPageFamily::Alleles => 1,
        SummaryPageFamily::Junctions => 2,
        SummaryPageFamily::Modifications => 3,
        SummaryPageFamily::Pairs => 4,
        SummaryPageFamily::FilterProvenance => 5,
    }
}

fn unique_summary_requests(
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    visible_only: bool,
) -> Result<Vec<OwnedSummaryPageRequest>, AlignmentSummaryGpuPageError> {
    let mut pages = Vec::new();
    for request in requests {
        pages.extend(summary_requests_for_scene(request, visible_only)?);
    }
    pages.sort_unstable_by_key(|page| page.key);
    pages.dedup_by_key(|page| page.key);
    Ok(pages)
}

fn summary_cursor_page(
    scenes: &[(
        Arc<AlignmentSceneSnapshot>,
        AlignmentSummaryGpuContentSelector,
        Option<Arc<SashimiPresentationTable>>,
    )],
    key: AlignmentSummaryGpuPageKey,
) -> Result<OwnedSummaryPageRequest, AlignmentSummaryGpuPrefetchCursorError> {
    for (scene, selector, presentation) in scenes {
        let Some(page) = scene.summary_page_for_key(key.semantic()) else {
            continue;
        };
        let companion_depth = match (selector, key.family()) {
            (AlignmentSummaryGpuContentSelector::Coverage { .. }, SummaryPageFamily::Depth)
                if key.companion_depth().is_none() =>
            {
                None
            }
            (
                AlignmentSummaryGpuContentSelector::Coverage { alleles: true },
                SummaryPageFamily::Alleles,
            ) => {
                let Some(companion_key) = key.companion_depth() else {
                    continue;
                };
                let Some(companion) = scene.summary_page_for_key(companion_key) else {
                    continue;
                };
                Some(Arc::clone(companion))
            }
            (
                AlignmentSummaryGpuContentSelector::Modifications,
                SummaryPageFamily::Modifications,
            ) if key.companion_depth().is_none() => None,
            (
                AlignmentSummaryGpuContentSelector::Junctions { .. },
                SummaryPageFamily::Junctions,
            ) if key.companion_depth().is_none() => None,
            _ => continue,
        };
        let candidate = OwnedSummaryPageRequest {
            key,
            page: Arc::clone(page),
            companion_depth,
            junction_presentation: (key.family() == SummaryPageFamily::Junctions)
                .then(|| presentation.clone())
                .flatten(),
        };
        if candidate.borrowed().key()? == key {
            return Ok(candidate);
        }
    }
    Err(AlignmentSummaryGpuPrefetchCursorError::PreparedAuthorityChanged)
}

fn borrowed_summary_requests(
    pages: &[OwnedSummaryPageRequest],
) -> Vec<AlignmentSummaryGpuPageRequest<'_>> {
    pages
        .iter()
        .map(OwnedSummaryPageRequest::borrowed)
        .collect()
}

fn page_interval_start(
    pages: &[AlignmentSummaryGpuPageRequest<'_>],
    key: AlignmentSummaryGpuPageKey,
) -> Option<u64> {
    pages
        .iter()
        .find(|request| request.key().ok() == Some(key))
        .map(|request| request.page.interval().start())
}

fn packed_buffer_capacity(
    packed: &PackedAlignmentSummaryPage,
) -> Result<usize, AlignmentSummaryGpuPageAdmissionError> {
    match packed {
        PackedAlignmentSummaryPage::Depth { instances, .. } => {
            summary_gpu_buffer_capacity::<AlignmentSummaryGpuDepthInstance>(instances.len())
        }
        PackedAlignmentSummaryPage::Alleles { instances } => {
            summary_gpu_buffer_capacity::<AlignmentSummaryGpuAlleleInstance>(instances.len())
        }
        PackedAlignmentSummaryPage::Modifications { instances, .. } => {
            summary_gpu_buffer_capacity::<AlignmentSummaryGpuModificationInstance>(instances.len())
        }
        PackedAlignmentSummaryPage::Junctions { instances, .. } => {
            summary_gpu_buffer_capacity::<AlignmentSummaryGpuJunctionInstance>(instances.len())
        }
    }
}

fn write_packed_page(
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    packed: &PackedAlignmentSummaryPage,
) {
    match packed {
        PackedAlignmentSummaryPage::Depth { instances, .. } if !instances.is_empty() => {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
        }
        PackedAlignmentSummaryPage::Alleles { instances } if !instances.is_empty() => {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
        }
        PackedAlignmentSummaryPage::Modifications { instances, .. } if !instances.is_empty() => {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
        }
        PackedAlignmentSummaryPage::Junctions { instances, .. } if !instances.is_empty() => {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
        }
        PackedAlignmentSummaryPage::Depth { .. }
        | PackedAlignmentSummaryPage::Alleles { .. }
        | PackedAlignmentSummaryPage::Modifications { .. }
        | PackedAlignmentSummaryPage::Junctions { .. } => {}
    }
}

fn summary_gpu_buffer_capacity<T>(
    count: usize,
) -> Result<usize, AlignmentSummaryGpuPageAdmissionError> {
    // Immutable summary pages are allocated (and charged to the ledger) at their exact size; see
    // `exact_immutable_buffer_bytes`.
    crate::alignment_scene::exact_immutable_buffer_bytes(count, size_of::<T>())
        .ok_or(AlignmentSummaryGpuPageAdmissionError::CapacityOverflow)
}

fn next_summary_capacity(count: usize) -> Result<usize, AlignmentSummaryGpuPageAdmissionError> {
    paged_cache::next_power_of_two_capacity(count)
        .ok_or(AlignmentSummaryGpuPageAdmissionError::CapacityOverflow)
}

/// Retained charge of a summary GPU page table of `capacity` entries; see
/// [`paged_cache::page_table_retained_bytes`].
fn summary_page_table_retained_bytes(capacity: usize) -> Option<usize> {
    paged_cache::page_table_retained_bytes::<GpuSummaryPage>(capacity)
}

fn take_summary_bundle_slot(
    next: &mut u32,
) -> Result<AllocationBundleSlot, AlignmentSummaryGpuReplacementError> {
    let slot = AllocationBundleSlot::new(*next);
    *next = next
        .checked_add(1)
        .ok_or(AlignmentSummaryGpuReplacementError::BundleSlotOverflow)?;
    Ok(slot)
}

fn exact_summary_retirement_ownership(
    allocation: &AllocationLease,
    owned_pin_counts: &BTreeMap<AllocationId, usize>,
) -> Result<bool, LedgerError> {
    let snapshot = allocation.snapshot()?;
    let owned_pins = owned_pin_counts.get(&allocation.id()).copied().unwrap_or(0);
    Ok(snapshot.references == owned_pins.saturating_add(1) && snapshot.pins == owned_pins)
}

fn summary_page_maximum_retained_bytes(
    request: &OwnedSummaryPageRequest,
) -> Result<usize, AlignmentSummaryGpuReplacementError> {
    let primitive_count = request.page.primitives().len();
    match request.key.family() {
        SummaryPageFamily::Depth => {
            let buffer =
                summary_gpu_buffer_capacity::<AlignmentSummaryGpuDepthInstance>(primitive_count)?;
            let entry_bytes = primitive_count
                .checked_mul(size_of::<DepthRangeEntry>())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            let tree_entries = if primitive_count == 0 {
                0
            } else {
                primitive_count
                    .checked_next_power_of_two()
                    .and_then(|count| count.checked_mul(2))
                    .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?
            };
            let tree_bytes = tree_entries
                .checked_mul(size_of::<u32>())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            buffer
                .checked_add(entry_bytes)
                .and_then(|bytes| bytes.checked_add(tree_bytes))
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
        }
        SummaryPageFamily::Alleles => {
            let maximum_instances = primitive_count
                .checked_mul(4)
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            Ok(summary_gpu_buffer_capacity::<
                AlignmentSummaryGpuAlleleInstance,
            >(maximum_instances)?)
        }
        SummaryPageFamily::Modifications => {
            let maximum_instances = primitive_count
                .checked_mul(3)
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            let buffer = summary_gpu_buffer_capacity::<AlignmentSummaryGpuModificationInstance>(
                maximum_instances,
            )?;
            let dictionary = primitive_count
                .min(u32::BITS as usize)
                .checked_mul(size_of::<ModificationCode>())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            buffer
                .checked_add(dictionary)
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
        }
        SummaryPageFamily::Junctions => {
            let maximum_instances = primitive_count
                .checked_mul(4)
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            let buffer = summary_gpu_buffer_capacity::<AlignmentSummaryGpuJunctionInstance>(
                maximum_instances,
            )?;
            let metadata = primitive_count
                .checked_mul(size_of::<AlignmentSummaryJunctionEntry>())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            let range_entries = if primitive_count == 0 {
                0
            } else {
                primitive_count
                    .checked_next_power_of_two()
                    .and_then(|count| count.checked_mul(2))
                    .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?
            };
            let range_index = range_entries
                .checked_mul(size_of::<JunctionRangeAggregate>())
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
            buffer
                .checked_add(metadata)
                .and_then(|bytes| bytes.checked_add(range_index))
                .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
        }
        family => Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family).into()),
    }
}

fn summary_page_maximum_packing_bytes(
    request: &OwnedSummaryPageRequest,
) -> Result<usize, AlignmentSummaryGpuReplacementError> {
    let retained = summary_page_maximum_retained_bytes(request)?;
    if request.key.family() == SummaryPageFamily::Modifications {
        let primitive_count = request.page.primitives().len();
        return retained
            .checked_add(
                primitive_count
                    .checked_mul(
                        size_of::<ModificationPackingCandidate>()
                            .saturating_mul(3)
                            .saturating_add(size_of::<(u64, u64)>()),
                    )
                    .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?,
            )
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow);
    }
    if request.key.family() != SummaryPageFamily::Alleles {
        return Ok(retained);
    }
    let depth_count = request
        .companion_depth
        .as_ref()
        .ok_or(AlignmentSummaryGpuPageError::MissingCompanionDepth(
            request.page.key(),
        ))?
        .primitives()
        .len();
    let entry_bytes = depth_count
        .checked_mul(size_of::<DepthRangeEntry>())
        .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
    let tree_entries = if depth_count == 0 {
        0
    } else {
        depth_count
            .checked_next_power_of_two()
            .and_then(|count| count.checked_mul(2))
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?
    };
    let tree_bytes = tree_entries
        .checked_mul(size_of::<u32>())
        .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
    retained
        .checked_add(entry_bytes)
        .and_then(|bytes| bytes.checked_add(tree_bytes))
        .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)
}

fn validate_device_buffer_size(
    device: &wgpu::Device,
    bytes: usize,
) -> Result<(), AlignmentSummaryGpuPageAdmissionError> {
    if bytes == 0 {
        return Ok(());
    }
    let limit = usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
    if bytes > limit {
        return Err(AlignmentSummaryGpuPageAdmissionError::DeviceBufferLimit {
            required: bytes,
            limit,
        });
    }
    Ok(())
}

fn aligned_uniform_stride(device: &wgpu::Device) -> usize {
    let alignment = device.limits().min_uniform_buffer_offset_alignment.max(1) as usize;
    size_of::<AlignmentSummaryGpuUniform>()
        .div_ceil(alignment)
        .saturating_mul(alignment)
}

fn create_summary_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("RGV alignment summary frame bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size_of::<AlignmentSummaryGpuUniform>() as u64),
            }),
        }],
    })
}

fn summary_uniform(
    camera: CpuSummaryCamera,
    screen_size: [f32; 2],
    page_start: u64,
    covered_clip_x: [f32; 2],
    scale_maximum: u32,
    minimum_alternate_fraction: f32,
    transform: CoverageScaleTransform,
    quality_weighted: bool,
    allele_interpretation: CoverageAlleleInterpretation,
    family: SummaryPageFamily,
    hidden_modification_mask: u32,
    modification_color_mode: ModifiedBaseColorMode,
    junction_scale_maximum: f32,
    junction_normalization: f32,
    junction_row_count: u32,
    minimum_junction_depth: u32,
    minimum_junction_flank: u32,
    junction_display_mode: JunctionDisplayMode,
    junction_labels: bool,
    overlays: AlignmentSummaryFrameOverlays,
) -> AlignmentSummaryGpuUniform {
    let (hovered_junction_span, hovered_component, hovered_present) =
        junction_interaction_uniform(overlays.hovered_junction, page_start);
    let (selected_junction_span, selected_component, selected_present) =
        junction_interaction_uniform(overlays.selected_junction, page_start);
    let (page_start_whole, page_start_fraction) =
        crate::shaders::split_camera_origin(page_start, camera.exact_start());
    AlignmentSummaryGpuUniform {
        screen_size: [screen_size[0].max(1.0), screen_size[1].max(1.0)],
        clip_origin: [camera.clip().x as f32, camera.clip().y as f32],
        clip_size: [camera.clip().width as f32, camera.clip().height as f32],
        points_per_base: split_f64(1.0 / camera.bases_per_pixel()),
        page_start_whole,
        page_start_fraction,
        covered_clip_x,
        scale_maximum: if family == SummaryPageFamily::Junctions {
            junction_scale_maximum.max(f32::EPSILON)
        } else {
            scale_maximum.max(1) as f32
        },
        minimum_alternate_fraction,
        transform: match transform {
            CoverageScaleTransform::Linear => 0,
            CoverageScaleTransform::SquareRoot => 1,
        },
        quality_weighted: u32::from(quality_weighted),
        family: match family {
            SummaryPageFamily::Depth => 0,
            SummaryPageFamily::Alleles => 1,
            SummaryPageFamily::Modifications => 2,
            SummaryPageFamily::Junctions => 3,
            _ => u32::MAX,
        },
        hidden_modification_mask,
        modification_color_mode: match modification_color_mode {
            ModifiedBaseColorMode::ByType => 0,
            ModifiedBaseColorMode::Mono => 1,
            ModifiedBaseColorMode::ModifiedVsUnmodified => 2,
        },
        junction_normalization,
        junction_row_count: junction_row_count.max(1),
        minimum_junction_depth,
        minimum_junction_flank,
        junction_display_mode: match junction_display_mode {
            JunctionDisplayMode::Collapsed => 0,
            JunctionDisplayMode::Expanded => 1,
        },
        allele_interpretation: match allele_interpretation {
            CoverageAlleleInterpretation::GenericReferenceAware => 0,
            CoverageAlleleInterpretation::Bisulfite(_) => 1,
        },
        bisulfite_context_mask: match allele_interpretation {
            CoverageAlleleInterpretation::GenericReferenceAware => 0,
            CoverageAlleleInterpretation::Bisulfite(context) => {
                u32::from(bisulfite_context_mask(context))
            }
        },
        junction_flags: u32::from(junction_labels),
        padding: 0,
        hovered_junction_span,
        selected_junction_span,
        junction_interaction: [
            hovered_component,
            u32::from(hovered_present),
            selected_component,
            u32::from(selected_present),
        ],
    }
}

fn junction_interaction_uniform(
    interaction: Option<AlignmentSummaryJunctionInteraction>,
    page_start: u64,
) -> ([f32; 4], u32, bool) {
    let Some(interaction) = interaction else {
        return ([0.0; 4], 0, false);
    };
    // Must be bit-identical to the packed instance offsets (the shader compares them exactly),
    // including the negative start of a junction entering the page from the left.
    let start = junction_page_offset(interaction.start, page_start);
    let end = junction_page_offset(interaction.end, page_start);
    (
        [start[0], start[1], end[0], end[1]],
        interaction.component as u32,
        interaction.start < interaction.end
            && interaction.component != AlignmentSummaryJunctionComponentToken::Annotation,
    )
}

/// Half the height of a junction label's chip, in points.
const JUNCTION_LABEL_HALF_HEIGHT: f64 = 6.0;

/// Width of a junction label's chip: the 10 pt text plus padding.
fn junction_label_width(text: &str) -> f64 {
    (text.chars().count() as f64) * 6.0 + 6.0
}

/// The arc a junction entry draws in `draw`, with its strand token; `None` when it is hidden,
/// filtered by depth or flank, or off screen.
fn project_drawn_junction(
    draw: &PreparedSummaryDraw,
    entry: AlignmentSummaryJunctionEntry,
) -> Option<(
    AlignmentSummaryGpuProjectedJunction,
    AlignmentSummaryJunctionComponentToken,
)> {
    let component = entry.drawn_component()?;
    let evidence = entry.evidence();
    let instance = AlignmentSummaryGpuJunctionInstance {
        start_offset: junction_page_offset(evidence.start, draw.page_start),
        end_offset: junction_page_offset(evidence.end, draw.page_start),
        evidence: [
            evidence.depth,
            evidence.depth,
            evidence.minimum_left_flank,
            evidence.minimum_right_flank,
        ],
        identity: [entry.expanded_row(), component as u32, 0, 0],
    };
    project_alignment_summary_junction_instance_with_labels(
        instance,
        draw.page_start,
        draw.camera,
        f64::from(draw.junction_scale_maximum),
        f64::from(draw.junction_normalization),
        draw.junction_row_count,
        draw.minimum_junction_depth,
        draw.minimum_junction_flank,
        draw.junction_display_mode,
        draw.show_junction_labels,
    )
    .map(|projected| (projected, component))
}

fn junction_count_label(
    evidence: JunctionEvidence,
    count_mode: SashimiCountMode,
    normalization: f64,
) -> String {
    match count_mode {
        SashimiCountMode::Raw => evidence.depth.to_string(),
        SashimiCountMode::CountsPerMillion => {
            let value = f64::from(evidence.depth) * normalization;
            if value < 10.0 {
                format!("{value:.1} CPM")
            } else {
                format!("{value:.0} CPM")
            }
        }
    }
}

const fn bisulfite_context_mask(context: BisulfiteContext) -> u8 {
    1 << match context {
        BisulfiteContext::Cg => 0,
        BisulfiteContext::Chg => 1,
        BisulfiteContext::Chh => 2,
        BisulfiteContext::Hcg => 3,
        BisulfiteContext::Gch => 4,
        BisulfiteContext::Wcg => 5,
    }
}

/// Signed page-relative junction coordinate as a double-single pair. A junction may start left
/// of its carrier page (an entering junction on the first prepared tile) and may end beyond it,
/// so the offset is formed as an exact integer difference before conversion; it stays exact for
/// any contig length because only the distance from the page start reaches floating point.
fn junction_page_offset(coordinate: u64, page_start: u64) -> [f32; 2] {
    let offset = if coordinate >= page_start {
        (coordinate - page_start) as f64
    } else {
        -((page_start - coordinate) as f64)
    };
    split_f64(offset)
}

fn split_f64(value: f64) -> [f32; 2] {
    let high = value as f32;
    [high, (value - f64::from(high)) as f32]
}

fn validate_summary_request(
    request: &AlignmentSummaryGpuSceneRequest<'_>,
) -> Result<(), AlignmentSummaryGpuPrepareError> {
    if request.screen_size_points[0] <= 0.0
        || request.screen_size_points[1] <= 0.0
        || !request.screen_size_points[0].is_finite()
        || !request.screen_size_points[1].is_finite()
        || request.scissor.width == 0
        || request.scissor.height == 0
    {
        return Err(AlignmentSummaryGpuPrepareError::InvalidFrameGeometry);
    }
    if let AlignmentSummaryGpuLaneOptions::Sashimi(options) = &request.options {
        let fixed = match options.scale {
            AlignmentSashimiScalePolicy::Fixed(maximum) => Some(maximum),
            AlignmentSashimiScalePolicy::Automatic | AlignmentSashimiScalePolicy::Shared(_) => {
                options.projection.scale_maximum
            }
        };
        if fixed.is_some_and(|maximum| !maximum.is_finite() || maximum <= 0.0) {
            return Err(AlignmentSummaryGpuPrepareError::InvalidJunctionScale);
        }
    }
    let _ = shared_fixed_scale(&request.options)?;
    Ok(())
}

fn shared_fixed_scale(
    options: &AlignmentSummaryGpuLaneOptions,
) -> Result<Option<u32>, AlignmentSummaryGpuPrepareError> {
    let AlignmentSummaryGpuLaneOptions::Coverage(options) = options else {
        return Ok(Some(1));
    };
    let Some(alleles) = options.alleles else {
        return Ok(options.depth.scale_maximum);
    };
    if !alleles.minimum_fraction.is_finite() || !(0.0..=1.0).contains(&alleles.minimum_fraction) {
        return Err(AlignmentSummaryGpuPrepareError::InvalidAlternateFraction);
    }
    if alleles.transform != options.depth.transform {
        return Err(AlignmentSummaryGpuPrepareError::IncompatibleAlleleScale);
    }
    match (options.depth.scale_maximum, alleles.scale_maximum) {
        (Some(depth), Some(allele)) if depth != allele => {
            Err(AlignmentSummaryGpuPrepareError::IncompatibleAlleleScale)
        }
        (Some(depth), _) => Ok(Some(depth)),
        (_, Some(allele)) => Ok(Some(allele)),
        (None, None) => Ok(None),
    }
}

#[derive(Clone, Copy, Debug)]
struct JunctionRequestMetrics {
    resolved_scale_maximum: f32,
    row_count: u32,
}

fn resolve_junction_request_metrics(
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    visible_pages: &[Vec<OwnedSummaryPageRequest>],
) -> Result<Vec<Option<JunctionRequestMetrics>>, AlignmentSummaryGpuPrepareError> {
    let mut measured = Vec::with_capacity(requests.len());
    let mut shared_maxima = BTreeMap::<(u64, u64, u8), f64>::new();
    for (request, pages) in requests.iter().zip(visible_pages) {
        let AlignmentSummaryGpuLaneOptions::Sashimi(options) = &request.options else {
            measured.push(None);
            continue;
        };
        let covered = summary_covered_interval(request);
        let mut maximum = 0.0_f64;
        let mut maximum_row = 0_u32;
        let mut any = false;
        for page in pages {
            let normalization = match options.projection.count_mode {
                SashimiCountMode::Raw => 1.0,
                SashimiCountMode::CountsPerMillion => page
                    .key
                    .semantic()
                    .junction_normalization_mapped_records()
                    .filter(|total| *total > 0)
                    .map(|total| 1_000_000.0 / total as f64)
                    .ok_or(AlignmentSummaryGpuPrepareError::MissingJunctionNormalization)?,
            };
            for primitive in page.page.primitives() {
                let SummarySemanticComponent::Junction(junction) = primitive.component else {
                    return Err(AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(
                        page.key,
                    ));
                };
                let start = page
                    .page
                    .interval()
                    .start()
                    .saturating_add(primitive.start_offset);
                let end = page
                    .page
                    .interval()
                    .start()
                    .saturating_add(primitive.end_offset);
                if covered.is_none_or(|covered| {
                    covered.contig() != page.page.interval().contig()
                        || start >= covered.end()
                        || end <= covered.start()
                }) || junction.evidence.depth < options.projection.minimum_depth
                    || junction.evidence.minimum_left_flank < options.projection.minimum_flank
                    || junction.evidence.minimum_right_flank < options.projection.minimum_flank
                {
                    continue;
                }
                any = true;
                maximum = maximum.max(f64::from(junction.evidence.depth) * normalization);
                maximum_row = maximum_row.max(junction.expanded_row);
            }
        }
        let row_count = if options.projection.display_mode == JunctionDisplayMode::Expanded {
            request
                .scene
                .summaries()
                .expanded_junction_row_count()
                .max(maximum_row.saturating_add(u32::from(any)))
                .max(1)
        } else {
            1
        };
        if let AlignmentSashimiScalePolicy::Shared(group) = options.scale {
            let mode = match options.projection.count_mode {
                SashimiCountMode::Raw => 0,
                SashimiCountMode::CountsPerMillion => 1,
            };
            shared_maxima
                .entry((request.coherence_scope, group.get(), mode))
                .and_modify(|shared| *shared = shared.max(maximum))
                .or_insert(maximum);
        }
        measured.push(Some((maximum, row_count)));
    }
    Ok(finalize_junction_request_metrics(
        requests,
        measured,
        &shared_maxima,
    ))
}

fn finalize_junction_request_metrics(
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    measured: Vec<Option<(f64, u32)>>,
    shared_maxima: &BTreeMap<(u64, u64, u8), f64>,
) -> Vec<Option<JunctionRequestMetrics>> {
    requests
        .iter()
        .zip(measured)
        .map(|(request, measured)| {
            let AlignmentSummaryGpuLaneOptions::Sashimi(options) = &request.options else {
                return None;
            };
            let (measured, row_count) = measured.expect("Sashimi request has measured metrics");
            let maximum = match options.scale {
                AlignmentSashimiScalePolicy::Automatic => measured,
                AlignmentSashimiScalePolicy::Fixed(maximum) => maximum,
                AlignmentSashimiScalePolicy::Shared(group) => {
                    let mode = match options.projection.count_mode {
                        SashimiCountMode::Raw => 0,
                        SashimiCountMode::CountsPerMillion => 1,
                    };
                    *shared_maxima
                        .get(&(request.coherence_scope, group.get(), mode))
                        .expect("shared Sashimi group was measured in this panel scope")
                }
            };
            Some(JunctionRequestMetrics {
                resolved_scale_maximum: maximum.max(f64::EPSILON) as f32,
                row_count,
            })
        })
        .collect()
}

fn resolve_resident_junction_request_metrics(
    renderer: &AlignmentSummaryGpuRenderer,
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    visible_pages: &[Vec<OwnedSummaryPageRequest>],
) -> Result<(Vec<Option<JunctionRequestMetrics>>, usize, usize), AlignmentSummaryGpuPrepareError> {
    let mut measured = Vec::with_capacity(requests.len());
    let mut shared_maxima = BTreeMap::<(u64, u64, u8), f64>::new();
    let mut range_queries = 0_usize;
    let mut examined_entries = 0_usize;
    for (request, pages) in requests.iter().zip(visible_pages) {
        let AlignmentSummaryGpuLaneOptions::Sashimi(options) = &request.options else {
            measured.push(None);
            continue;
        };
        let covered = summary_covered_interval(request);
        let mut maximum = 0.0_f64;
        let mut maximum_row = 0_u32;
        let mut any = false;
        for page_request in pages {
            let page = renderer.page(page_request.key).ok_or(
                AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(page_request.key),
            )?;
            let GpuSummaryPageMetadata::Junctions(metadata) = &page.metadata else {
                return Err(AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(
                    page_request.key,
                ));
            };
            let Some(covered) = covered else {
                continue;
            };
            let normalization = metadata
                .normalization_factor(options.projection.count_mode)
                .ok_or(AlignmentSummaryGpuPrepareError::MissingJunctionNormalization)?;
            let query = metadata.range_index.query(
                &metadata.entries,
                covered.start().saturating_sub(page.interval_start),
                covered.end().saturating_sub(page.interval_start),
                options.projection.minimum_depth,
                options.projection.minimum_flank,
            );
            range_queries = range_queries.saturating_add(1);
            examined_entries = examined_entries.saturating_add(query.examined_entries);
            if query.matched > 0 {
                any = true;
                maximum = maximum.max(f64::from(query.maximum_depth) * normalization);
                maximum_row = maximum_row.max(query.maximum_expanded_row);
            }
        }
        let row_count = if options.projection.display_mode == JunctionDisplayMode::Expanded {
            request
                .scene
                .summaries()
                .expanded_junction_row_count()
                .max(maximum_row.saturating_add(u32::from(any)))
                .max(1)
        } else {
            1
        };
        if let AlignmentSashimiScalePolicy::Shared(group) = options.scale {
            let mode = match options.projection.count_mode {
                SashimiCountMode::Raw => 0,
                SashimiCountMode::CountsPerMillion => 1,
            };
            shared_maxima
                .entry((request.coherence_scope, group.get(), mode))
                .and_modify(|shared| *shared = shared.max(maximum))
                .or_insert(maximum);
        }
        measured.push(Some((maximum, row_count)));
    }
    Ok((
        finalize_junction_request_metrics(requests, measured, &shared_maxima),
        range_queries,
        examined_entries,
    ))
}

fn summary_draw_style(
    options: &AlignmentSummaryGpuLaneOptions,
    key: AlignmentSummaryGpuPageKey,
    metadata: &GpuSummaryPageMetadata,
    instance_count: u32,
) -> Result<SummaryDrawStyle, AlignmentSummaryGpuPrepareError> {
    match (options, key.family(), metadata) {
        (
            AlignmentSummaryGpuLaneOptions::Coverage(options),
            SummaryPageFamily::Depth,
            GpuSummaryPageMetadata::Depth(_),
        )
        | (
            AlignmentSummaryGpuLaneOptions::Coverage(options),
            SummaryPageFamily::Alleles,
            GpuSummaryPageMetadata::Alleles,
        ) => {
            let allele = options.alleles.unwrap_or_default();
            Ok(SummaryDrawStyle {
                minimum_alternate_fraction: allele.minimum_fraction as f32,
                transform: options.depth.transform,
                quality_weighted: options
                    .alleles
                    .is_some_and(|alleles| alleles.quality_weighted),
                allele_interpretation: options.alleles.map_or(
                    CoverageAlleleInterpretation::GenericReferenceAware,
                    |alleles| alleles.interpretation,
                ),
                hidden_modification_mask: 0,
                modification_color_mode: ModifiedBaseColorMode::ByType,
                junction_normalization: 1.0,
                junction_row_count: 1,
                minimum_junction_depth: 0,
                minimum_junction_flank: 0,
                junction_display_mode: JunctionDisplayMode::Collapsed,
                background_range: 0..0,
                foreground_range: 0..instance_count,
            })
        }
        (
            AlignmentSummaryGpuLaneOptions::Modifications(options),
            SummaryPageFamily::Modifications,
            GpuSummaryPageMetadata::Modifications(metadata),
        ) => {
            let background_range = match options.color_mode {
                ModifiedBaseColorMode::ModifiedVsUnmodified => metadata.background_range(),
                ModifiedBaseColorMode::ByType | ModifiedBaseColorMode::Mono => 0..0,
            };
            let foreground_range = if options.projection.probability_weighted {
                metadata.weighted_range()
            } else {
                metadata.raw_range()
            };
            if background_range.end > instance_count || foreground_range.end > instance_count {
                return Err(AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(
                    key,
                ));
            }
            Ok(SummaryDrawStyle {
                minimum_alternate_fraction: 0.0,
                transform: CoverageScaleTransform::Linear,
                quality_weighted: options.projection.probability_weighted,
                allele_interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
                hidden_modification_mask: metadata
                    .hidden_code_mask(&options.projection.hidden_codes),
                modification_color_mode: options.color_mode,
                junction_normalization: 1.0,
                junction_row_count: 1,
                minimum_junction_depth: 0,
                minimum_junction_flank: 0,
                junction_display_mode: JunctionDisplayMode::Collapsed,
                background_range,
                foreground_range,
            })
        }
        (
            AlignmentSummaryGpuLaneOptions::Sashimi(options),
            SummaryPageFamily::Junctions,
            GpuSummaryPageMetadata::Junctions(metadata),
        ) => Ok(SummaryDrawStyle {
            minimum_alternate_fraction: 0.0,
            transform: CoverageScaleTransform::Linear,
            quality_weighted: false,
            allele_interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
            hidden_modification_mask: 0,
            modification_color_mode: ModifiedBaseColorMode::ByType,
            junction_normalization: metadata
                .normalization_factor(options.projection.count_mode)
                .ok_or(AlignmentSummaryGpuPrepareError::MissingJunctionNormalization)?
                as f32,
            junction_row_count: metadata.maximum_expanded_row.saturating_add(1).max(1),
            minimum_junction_depth: options.projection.minimum_depth,
            minimum_junction_flank: options.projection.minimum_flank,
            junction_display_mode: options.projection.display_mode,
            background_range: 0..0,
            foreground_range: 0..instance_count,
        }),
        _ => Err(AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(
            key,
        )),
    }
}

fn summary_overlay_count(request: &AlignmentSummaryGpuSceneRequest<'_>) -> usize {
    if summary_covered_interval(request).is_none() {
        return 0;
    }
    let (center, option_selected) = match &request.options {
        AlignmentSummaryGpuLaneOptions::Coverage(options) => (
            usize::from(options.depth.show_center_line),
            options.depth.selected_position,
        ),
        AlignmentSummaryGpuLaneOptions::Modifications(options) => (0, options.selected_position),
        AlignmentSummaryGpuLaneOptions::Sashimi(options) => (
            match options.projection.display_mode {
                JunctionDisplayMode::Collapsed => 1,
                JunctionDisplayMode::Expanded => {
                    request.scene.summaries().expanded_junction_row_count() as usize
                }
            },
            None,
        ),
    };
    let selected_position = request.overlays.selected_position.or(option_selected);
    let selected = usize::from(
        selected_position.is_some_and(|position| request.camera.interval().contains(position)),
    );
    center
        .saturating_add(selected)
        .saturating_add(summary_insertion_mark_rects(request).len())
}

/// IGV-style insertion marks for a coverage lane with per-base allele detail. Counted and
/// appended through this one function so the overlay count always matches what is emitted.
fn summary_insertion_mark_rects(
    request: &AlignmentSummaryGpuSceneRequest<'_>,
) -> Vec<InsertionMarkRect> {
    let AlignmentSummaryGpuLaneOptions::Coverage(options) = &request.options else {
        return Vec::new();
    };
    let (Some(alleles), Some(covered)) = (options.alleles, summary_covered_interval(request))
    else {
        return Vec::new();
    };
    let marks = coverage_insertion_marks(request.scene, covered, alleles.minimum_fraction);
    insertion_mark_rects(request.camera, &marks)
}

fn append_summary_overlays(
    request: &AlignmentSummaryGpuSceneRequest<'_>,
    overlays: &mut Vec<AlignmentSummaryGpuOverlayInstance>,
) {
    if summary_covered_interval(request).is_none() {
        return;
    }
    let clip = request.camera.clip();
    if let AlignmentSummaryGpuLaneOptions::Sashimi(options) = &request.options {
        let row_count = match options.projection.display_mode {
            JunctionDisplayMode::Collapsed => 1,
            JunctionDisplayMode::Expanded => request
                .scene
                .summaries()
                .expanded_junction_row_count()
                .max(1),
        };
        let band_height = clip.height / f64::from(row_count);
        for row in 0..row_count {
            overlays.push(AlignmentSummaryGpuOverlayInstance::new(
                [
                    clip.x as f32,
                    (clip.y + (f64::from(row) + 0.5) * band_height) as f32,
                ],
                [clip.width as f32, 1.0],
                PaletteToken::JUNCTION_BASELINE,
            ));
        }
    }
    for rect in summary_insertion_mark_rects(request) {
        overlays.push(AlignmentSummaryGpuOverlayInstance::new(
            rect.origin,
            rect.size,
            PaletteToken::INSERTION,
        ));
    }
    if matches!(
        &request.options,
        AlignmentSummaryGpuLaneOptions::Coverage(options) if options.depth.show_center_line
    ) {
        overlays.push(AlignmentSummaryGpuOverlayInstance::new(
            [(clip.x + clip.width * 0.5) as f32, clip.y as f32],
            [1.0, clip.height as f32],
            PaletteToken::COVERAGE_CENTER_LINE,
        ));
    }
    let option_selected = match &request.options {
        AlignmentSummaryGpuLaneOptions::Coverage(options) => options.depth.selected_position,
        AlignmentSummaryGpuLaneOptions::Modifications(options) => options.selected_position,
        AlignmentSummaryGpuLaneOptions::Sashimi(_) => None,
    };
    if let Some(position) = request.overlays.selected_position.or(option_selected)
        && request.camera.interval().contains(position)
    {
        let exact_start = request.camera.exact_start();
        overlays.push(AlignmentSummaryGpuOverlayInstance::new(
            [
                (clip.x + (position as f64 - exact_start) / request.camera.bases_per_pixel())
                    as f32,
                clip.y as f32,
            ],
            [
                (1.0 / request.camera.bases_per_pixel()).clamp(2.0, 12.0) as f32,
                clip.height as f32,
            ],
            match request.options {
                AlignmentSummaryGpuLaneOptions::Coverage(_) => {
                    PaletteToken::COVERAGE_SELECTED_POSITION
                }
                AlignmentSummaryGpuLaneOptions::Modifications(_) => {
                    PaletteToken::MODIFICATION_SELECTED_POSITION
                }
                AlignmentSummaryGpuLaneOptions::Sashimi(_) => {
                    PaletteToken::SASHIMI_SELECTED_POSITION
                }
            },
        ));
    }
}

fn summary_covered_interval(
    request: &AlignmentSummaryGpuSceneRequest<'_>,
) -> Option<GenomicInterval> {
    intersect_summary_intervals(request.camera.interval(), request.scene.prepared_interval())
}

fn intersect_summary_intervals(
    left: GenomicInterval,
    right: GenomicInterval,
) -> Option<GenomicInterval> {
    if left.contig() != right.contig() {
        return None;
    }
    let start = left.start().max(right.start());
    let end = left.end().min(right.end());
    (start < end).then(|| {
        GenomicInterval::new(left.contig(), start, end)
            .expect("intersection of valid genomic intervals is valid")
    })
}

fn summary_covered_clip_x(camera: CpuSummaryCamera, covered: GenomicInterval) -> [f32; 2] {
    let clip = camera.clip();
    let left = (clip.x
        + (covered.start() as f64 - camera.exact_start()) / camera.bases_per_pixel())
    .clamp(clip.x, clip.x + clip.width);
    let right = (clip.x + (covered.end() as f64 - camera.exact_start()) / camera.bases_per_pixel())
        .clamp(left, clip.x + clip.width);
    [left as f32, right as f32]
}

fn alignment_summary_gpu_frame_bytes(
    scene_revisions: usize,
    page_manifest: usize,
    camera_sequences: usize,
    scales: usize,
    draws: usize,
    overlay_draws: usize,
    pins: usize,
) -> usize {
    const ARC_SLICE_HEADER_WORDS: usize = 2;
    let arc_header = ARC_SLICE_HEADER_WORDS.saturating_mul(size_of::<usize>());
    size_of::<AlignmentSummaryGpuFrame>()
        .saturating_add(
            scene_revisions
                .saturating_mul(size_of::<SceneRevision>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            scene_revisions
                .saturating_mul(size_of::<AllocationId>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            scene_revisions
                .saturating_mul(size_of::<AlignmentSummaryGpuContentSelector>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            page_manifest
                .saturating_mul(size_of::<AlignmentSummaryGpuPageKey>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            camera_sequences
                .saturating_mul(size_of::<u64>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            scales
                .saturating_mul(size_of::<u32>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            draws
                .saturating_mul(size_of::<PreparedSummaryDraw>())
                .saturating_add(arc_header),
        )
        .saturating_add(
            overlay_draws
                .saturating_mul(size_of::<PreparedSummaryOverlayDraw>())
                .saturating_add(arc_header),
        )
        .saturating_add(pins.saturating_mul(size_of::<PinnedAllocationLease>()))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSummaryGpuContentReplacementReason {
    #[error(
        "content authority belongs to device generation {authority}, renderer is generation {renderer}"
    )]
    DeviceGeneration { authority: u64, renderer: u64 },
    #[error(
        "ordered summary scene content changed (authority has {authority_scenes} scenes, request has {requested_scenes})"
    )]
    SceneContent {
        authority_scenes: usize,
        requested_scenes: usize,
    },
    #[error(
        "summary lane content contract changed (authority has {authority_lanes} lanes, request has {requested_lanes})"
    )]
    ContentContract {
        authority_lanes: usize,
        requested_lanes: usize,
    },
    #[error(
        "prepared summary-page manifest changed (authority has {authority_pages} pages, request has {requested_pages})"
    )]
    PageManifest {
        authority_pages: usize,
        requested_pages: usize,
    },
    #[error(
        "{missing} summary pages required by the camera are not resident; first missing page is {first_missing:?}"
    )]
    MissingResidentPages {
        missing: usize,
        first_missing: AlignmentSummaryGpuPageKey,
    },
    #[error(
        "summary camera frame needs {required} retained bytes but the content replacement provisioned {available}"
    )]
    FrameEnvelope { required: usize, available: usize },
}

#[derive(Debug, Error)]
pub enum AlignmentSummaryGpuCameraCaptureError {
    #[error("alignment summary GPU content replacement is required: {reason}")]
    ContentReplacementRequired {
        reason: AlignmentSummaryGpuContentReplacementReason,
    },
    #[error("all {slots} reusable summary camera frame slots are in flight")]
    FrameSlotBusy { slots: usize },
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Prepare(#[from] AlignmentSummaryGpuPrepareError),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSummaryGpuPageError {
    #[error("summary family {0:?} is not supported by the exact summary GPU renderer")]
    UnsupportedFamily(SummaryPageFamily),
    #[error("summary GPU modification page contains {0} distinct codes; at most 32 are supported")]
    TooManyModificationCodes(usize),
    #[error("summary GPU semantic page capacity overflowed usize or u32")]
    CapacityOverflow,
    #[error("allele summary page {0:?} has no exact companion depth page")]
    MissingCompanionDepth(SummaryPageKey),
    #[error("allele summary page {allele:?} is incompatible with depth page {depth:?}")]
    CompanionDepthMismatch {
        allele: SummaryPageKey,
        depth: SummaryPageKey,
    },
    #[error("allele summary page {0:?} does not carry reference-aware substitutions")]
    MissingReferenceBase(SummaryPageKey),
    #[error("allele summary page {0:?} does not carry exact bisulfite reference context")]
    MissingReferenceContext(SummaryPageKey),
    #[error("summary page {0:?} contains a component from another family")]
    UnexpectedSemanticComponent(SummaryPageKey),
    #[error("summary page {page:?} has invalid semantic span {start}..{end}")]
    InvalidSemanticSpan {
        page: SummaryPageKey,
        start: u64,
        end: u64,
    },
    #[error(
        "summary page {page:?} spans page offsets {start}..{end}, beyond the {limit}-base range \
         a GPU page can address"
    )]
    SemanticOffsetOutOfRange {
        page: SummaryPageKey,
        start: u64,
        end: u64,
        limit: u64,
    },
    #[error("depth summary page {0:?} has overlapping ranges and cannot be indexed exactly")]
    OverlappingDepthSpans(SummaryPageKey),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSummaryGpuPageAdmissionError {
    #[error("summary GPU batch contains duplicate page {0:?}")]
    DuplicatePageKey(AlignmentSummaryGpuPageKey),
    #[error("summary GPU page has {0} instances, exceeding the u32 draw address space")]
    TooManyInstances(usize),
    #[error("summary GPU page capacity overflowed usize")]
    CapacityOverflow,
    #[error("summary GPU page table capacity is invalid: required {required}, allocated {actual}")]
    PageTableCapacity { required: usize, actual: usize },
    #[error("summary GPU buffer requires {required} bytes but the device limit is {limit}")]
    DeviceBufferLimit { required: usize, limit: usize },
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

impl From<PageTableError> for AlignmentSummaryGpuPageAdmissionError {
    fn from(error: PageTableError) -> Self {
        match error {
            PageTableError::CapacityOverflow => Self::CapacityOverflow,
            PageTableError::Capacity { required, actual } => {
                Self::PageTableCapacity { required, actual }
            }
            PageTableError::Ledger(error) => Self::from(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSummaryGpuResidencyError {
    #[error(
        "{missing} prepared summary GPU pages are not resident; first missing page is {first_missing:?}"
    )]
    MissingPreparedPages {
        missing: usize,
        first_missing: AlignmentSummaryGpuPageKey,
    },
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSummaryGpuPrefetchCursorError {
    #[error("prepared summary GPU authority changed before prefetch began")]
    PreparedAuthorityChanged,
    #[error(
        "summary GPU prefetch cursor belongs to device generation {cursor}, renderer is generation {renderer}"
    )]
    DeviceGeneration { cursor: u64, renderer: u64 },
    #[error("summary GPU prefetch cursor still has {remaining} pages to admit")]
    Incomplete { remaining: usize },
    #[error(
        "complete summary GPU residency exceeds the optional pin ceiling after retaining {retained_bytes} bytes (aggregate pinned ceiling {maximum_total_pinned_bytes})"
    )]
    ResidencyBudgetLimited {
        retained_bytes: usize,
        maximum_total_pinned_bytes: usize,
    },
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Admission(#[from] AlignmentSummaryGpuPageAdmissionError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[derive(Debug, Error)]
pub enum AlignmentSummaryGpuPrepareError {
    #[error("summary GPU frame geometry is invalid")]
    InvalidFrameGeometry,
    #[error("summary allele threshold must be finite and in 0..=1")]
    InvalidAlternateFraction,
    #[error("depth and allele lanes must use one shared scale maximum and transform")]
    IncompatibleAlleleScale,
    #[error("CPM Sashimi rendering requires an exact whole-file mapped-read normalization")]
    MissingJunctionNormalization,
    #[error("Sashimi scale maximum must be finite and positive")]
    InvalidJunctionScale,
    #[error("{missing} mandatory summary GPU pages are not resident")]
    MissingMandatoryPages { missing: usize },
    #[error("summary overlay count exceeds the u32 draw address space")]
    TooManyOverlayPrimitives,
    #[error("summary dynamic-uniform offset exceeds the u32 address space")]
    DynamicUniformAddressSpace,
    #[error(
        "summary camera frame requires {required} retained bytes but the reusable slot provides {available}"
    )]
    FrameSlotCapacity { required: usize, available: usize },
    #[error("all {slots} reusable summary camera frame slots are in flight")]
    FrameSlotBusy { slots: usize },
    #[error("resident summary GPU page {0:?} has incompatible range metadata")]
    ResidentMetadataMismatch(AlignmentSummaryGpuPageKey),
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Admission(#[from] AlignmentSummaryGpuPageAdmissionError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[derive(Debug, Error)]
pub enum AlignmentSummaryGpuFrameBuildError {
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Admission(#[from] AlignmentSummaryGpuPageAdmissionError),
    #[error(transparent)]
    Prepare(#[from] AlignmentSummaryGpuPrepareError),
}

#[derive(Debug, Error)]
pub enum AlignmentSummaryGpuReplacementError {
    #[error("summary replacement bundle slot space is exhausted")]
    BundleSlotOverflow,
    #[error("summary replacement capacity arithmetic overflowed")]
    CapacityOverflow,
    #[error(
        "summary replacement was planned for device generation {planned}, current is {current}"
    )]
    DeviceGeneration { planned: u64, current: u64 },
    #[error("a prior summary frame belongs to another device generation")]
    PriorFrameDeviceGeneration,
    #[error(
        "summary replacement plan is stale (planned revision {planned_revision}, current {current_revision})"
    )]
    StaleRenderer {
        planned_revision: u64,
        current_revision: u64,
    },
    #[error("summary retirement cannot be restored over an existing page-table allocation")]
    RetirementRestoreConflict,
    #[error(
        "summary replacement bundle policy revision differs (planned {planned}, reserved {reserved})"
    )]
    BundlePolicyRevision { planned: u64, reserved: u64 },
    #[error("summary requests changed after their replacement plan was built")]
    RequestChangedAfterPlan,
    #[error("staged summary bundle actuals do not exactly match their planned demands")]
    ActualDemandMismatch,
    #[error("staged summary page {key:?} retained {actual} bytes, above planned maximum {maximum}")]
    PageCapacityExceeded {
        key: AlignmentSummaryGpuPageKey,
        actual: usize,
        maximum: usize,
    },
    #[error("staged summary page table retained {actual} bytes, above planned maximum {maximum}")]
    PageTableCapacityExceeded { actual: usize, maximum: usize },
    #[error("staged summary frame retained {actual} bytes, above planned maximum {maximum}")]
    FrameCapacityExceeded { actual: usize, maximum: usize },
    #[error(transparent)]
    Bundle(#[from] AllocationBundleError),
    #[error(transparent)]
    Page(#[from] AlignmentSummaryGpuPageError),
    #[error(transparent)]
    Admission(#[from] AlignmentSummaryGpuPageAdmissionError),
    #[error(transparent)]
    Prepare(#[from] AlignmentSummaryGpuPrepareError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

impl AlignmentSummaryGpuFrameBuildError {
    /// Persistent retained-budget pressure whose failed reservation may succeed after
    /// cooperative cache reclaim. The reported limit preserves whether transition reserve was
    /// available. Semantic, device-limit, transient-budget, and malformed-page failures must not
    /// trigger cross-cache eviction.
    #[must_use]
    pub const fn persistent_budget_pressure(&self) -> Option<GpuBudgetPressure> {
        match self {
            Self::Page(_) => None,
            Self::Admission(error) => summary_admission_budget_pressure(error),
            Self::Prepare(error) => summary_prepare_budget_pressure(error),
        }
    }
}

const fn summary_admission_budget_pressure(
    error: &AlignmentSummaryGpuPageAdmissionError,
) -> Option<GpuBudgetPressure> {
    match error {
        AlignmentSummaryGpuPageAdmissionError::Ledger(LedgerError::BudgetExceeded {
            requested,
            charged,
            reserved,
            limit,
        }) => Some(GpuBudgetPressure::from_budget_failure(
            *requested, *charged, *reserved, *limit,
        )),
        _ => None,
    }
}

const fn summary_prepare_budget_pressure(
    error: &AlignmentSummaryGpuPrepareError,
) -> Option<GpuBudgetPressure> {
    match error {
        AlignmentSummaryGpuPrepareError::Admission(error) => {
            summary_admission_budget_pressure(error)
        }
        AlignmentSummaryGpuPrepareError::Ledger(LedgerError::BudgetExceeded {
            requested,
            charged,
            reserved,
            limit,
        }) => Some(GpuBudgetPressure::from_budget_failure(
            *requested, *charged, *reserved, *limit,
        )),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSummaryGpuRenderError {
    #[error(
        "GPU summary frame belongs to device generation {frame}; renderer is at generation {renderer}"
    )]
    DeviceGenerationMismatch { frame: u64, renderer: u64 },
    #[error("GPU summary frame has no uniform buffer")]
    MissingUniformBuffer,
    #[error("GPU summary frame has no overlay buffer")]
    MissingOverlayBuffer,
    #[error("a pinned GPU summary page disappeared before render: {0:?}")]
    MissingPinnedPage(AlignmentSummaryGpuPageKey),
    #[error("summary render encountered unsupported resident family {0:?}")]
    UnsupportedFamily(SummaryPageFamily),
}

#[cfg(test)]
pub(crate) mod tests;
