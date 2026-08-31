use std::{
    collections::{BTreeMap, BTreeSet},
    mem::{size_of, size_of_val},
    num::{NonZeroU64, NonZeroUsize},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use bytemuck::{Pod, Zeroable};
use rgv_alignment_scene::{
    AlignmentCoverageLaneOptions, AlignmentModificationLaneOptions, AlignmentSceneSnapshot,
    AllocationBundleActual, AllocationBundleDemand, AllocationBundleError, AllocationBundleLease,
    AllocationBundleReservation, AllocationBundleSlot, AllocationClass, AllocationId,
    AllocationLease, AllocationLedger, AllocationRetirementClaimSet, AllocationRetirementToken,
    CoverageAlleleInterpretation, CpuSummaryCamera, LedgerError, LedgerProgress,
    PinnedAllocationLease, ReservationKind, SceneRevision, SummaryPageFamily, SummaryPageKey,
    SummarySemanticComponent, SummarySemanticPage, TransientResourceClass,
};
use rgv_core::{CoverageScaleTransform, GenomicInterval, ModifiedBaseColorMode};
use rgv_hts::{ModificationBin, ModificationCode, ModificationSeriesKey};
use rgv_layout::modification_code_color;
use thiserror::Error;

use crate::{
    AlignmentGpuScissor, AlignmentLedgerPolicyView, GpuBudgetPressure, GpuReclaimGoal,
    GpuReclaimReport,
};

const SHADER: &str = include_str!("alignment_summary.wgsl");

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
        })
    }

    #[must_use]
    pub const fn semantic(self) -> SummaryPageKey {
        self.semantic
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
}

impl<'a> AlignmentSummaryGpuPageRequest<'a> {
    #[must_use]
    pub const fn depth(page: &'a Arc<SummarySemanticPage>) -> Self {
        Self {
            page,
            companion_depth: None,
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
        }
    }

    #[must_use]
    pub const fn modifications(page: &'a Arc<SummarySemanticPage>) -> Self {
        Self {
            page,
            companion_depth: None,
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
            family => Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family)),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuDepthInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    pub depth: u32,
    padding: [u32; 3],
}

impl AlignmentSummaryGpuDepthInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AlignmentSummaryAlleleToken {
    A = 0,
    C = 1,
    G = 2,
    T = 3,
    N = 4,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuAlleleInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    pub local_depth: u32,
    pub allele: u32,
    /// `[fraction, aggregate alternate fraction, preceding stack fraction]`.
    pub raw: [f32; 3],
    /// `[fraction, aggregate alternate fraction, preceding stack fraction]`.
    pub quality_weighted: [f32; 3],
}

impl AlignmentSummaryGpuAlleleInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32,
            3 => Uint32,
            4 => Float32x3,
            5 => Float32x3
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

/// One immutable modification-frequency candidate. A page stores two camera-independent
/// orderings of these instances (raw and probability-weighted). `later_code_mask` names every
/// better candidate at the same semantic bin; the shader therefore emits exactly the last
/// non-hidden candidate without repacking when lane filters change.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuModificationInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    /// `[raw modified fraction, probability-weighted modified fraction]`.
    pub fractions: [f32; 2],
    pub by_type_color: [f32; 4],
    pub code_index: u32,
    pub later_code_mask: u32,
    pub modified_calls: u32,
    pub canonical_observations: u32,
    /// `0` is a modification bar and `1` is the optional unmodified-background band.
    pub shape: u32,
    padding: [u32; 3],
}

impl AlignmentSummaryGpuModificationInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x2,
            3 => Float32x4,
            4 => Uint32x4,
            5 => Uint32
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct AlignmentSummaryGpuOverlayInstance {
    origin: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

impl AlignmentSummaryGpuOverlayInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x4
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedAllele {
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub allele: AlignmentSummaryAlleleToken,
    pub fraction: f64,
    pub aggregate_alternate_fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedModification {
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub fraction: f64,
    pub code_index: u32,
    pub modified_calls: u32,
    pub canonical_observations: u32,
}

#[must_use]
pub fn project_alignment_summary_depth_instance(
    instance: AlignmentSummaryGpuDepthInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> Option<AlignmentSummaryGpuProjectedBounds> {
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let height = (transformed_depth_fraction(instance.depth, scale_maximum, transform)
        * camera.clip().height)
        .max(1.0);
    Some(AlignmentSummaryGpuProjectedBounds {
        x,
        y: camera.clip().y + camera.clip().height - height,
        width,
        height,
    })
}

#[must_use]
pub fn project_alignment_summary_allele_instance(
    instance: AlignmentSummaryGpuAlleleInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
    minimum_alternate_fraction: f64,
    quality_weighted: bool,
) -> Option<AlignmentSummaryGpuProjectedAllele> {
    let values = if quality_weighted {
        instance.quality_weighted
    } else {
        instance.raw
    };
    let fraction = f64::from(values[0]);
    let aggregate = f64::from(values[1]);
    let preceding = f64::from(values[2]);
    if instance.local_depth == 0 || fraction <= 0.0 || aggregate < minimum_alternate_fraction {
        return None;
    }
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let coverage_height =
        (transformed_depth_fraction(instance.local_depth, scale_maximum, transform)
            * camera.clip().height)
            .max(1.0);
    let semantic_height = fraction * coverage_height;
    let height = semantic_height.max(1.0).min(coverage_height);
    let y = (camera.clip().y + camera.clip().height - (preceding + fraction) * coverage_height)
        .max(camera.clip().y);
    Some(AlignmentSummaryGpuProjectedAllele {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y,
            width,
            height,
        },
        allele: allele_token(instance.allele)?,
        fraction,
        aggregate_alternate_fraction: aggregate,
    })
}

/// CPU oracle for the modification vertex contract. `hidden_code_mask` is page-local and is
/// derived from the immutable page code dictionary at frame construction; it is not part of the
/// page key. Only the exact dominant non-hidden series at a semantic bin projects a bar.
#[must_use]
pub fn project_alignment_summary_modification_instance(
    instance: AlignmentSummaryGpuModificationInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    probability_weighted: bool,
    hidden_code_mask: u32,
) -> Option<AlignmentSummaryGpuProjectedModification> {
    if instance.shape != 0
        || instance.modified_calls == 0
        || instance.code_index >= u32::BITS
        || hidden_code_mask & (1_u32 << instance.code_index) != 0
        || instance.later_code_mask & !hidden_code_mask != 0
    {
        return None;
    }
    let fraction = f64::from(instance.fractions[usize::from(probability_weighted)]).clamp(0.0, 1.0);
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let height = (fraction * camera.clip().height).max(1.0);
    Some(AlignmentSummaryGpuProjectedModification {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y: camera.clip().y + camera.clip().height - height,
            width,
            height,
        },
        fraction,
        code_index: instance.code_index,
        modified_calls: instance.modified_calls,
        canonical_observations: instance.canonical_observations,
    })
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
    let start_offset = f64::from(start_offset[0]) + f64::from(start_offset[1]);
    let end_offset = f64::from(end_offset[0]) + f64::from(end_offset[1]);
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

const fn allele_token(value: u32) -> Option<AlignmentSummaryAlleleToken> {
    match value {
        0 => Some(AlignmentSummaryAlleleToken::A),
        1 => Some(AlignmentSummaryAlleleToken::C),
        2 => Some(AlignmentSummaryAlleleToken::G),
        3 => Some(AlignmentSummaryAlleleToken::T),
        4 => Some(AlignmentSummaryAlleleToken::N),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
struct DepthRangeEntry {
    start: u64,
    end: u64,
    depth: u32,
}

/// Reusable exact range-maximum metadata built once when a depth page is admitted. Queries use a
/// segment tree and inspect no semantic primitives, including for fractional viewport edges.
#[derive(Clone, Debug, Default)]
pub struct AlignmentSummaryDepthRangeIndex {
    entries: Vec<DepthRangeEntry>,
    maximum_tree: Vec<u32>,
    leaf_base: usize,
}

impl AlignmentSummaryDepthRangeIndex {
    fn build(page: &SummarySemanticPage) -> Result<Self, AlignmentSummaryGpuPageError> {
        if page.key().family() != SummaryPageFamily::Depth {
            return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(
                page.key().family(),
            ));
        }
        let mut entries = Vec::with_capacity(page.primitives().len());
        for primitive in page.primitives() {
            let SummarySemanticComponent::Depth { depth, .. } = primitive.component else {
                return Err(AlignmentSummaryGpuPageError::UnexpectedSemanticComponent(
                    page.key(),
                ));
            };
            if primitive.start_offset >= primitive.end_offset {
                return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                    page: page.key(),
                    start: primitive.start_offset,
                    end: primitive.end_offset,
                });
            }
            entries.push(DepthRangeEntry {
                start: page
                    .interval()
                    .start()
                    .saturating_add(primitive.start_offset),
                end: page.interval().start().saturating_add(primitive.end_offset),
                depth,
            });
        }
        entries.sort_unstable_by_key(|entry| (entry.start, entry.end));
        if entries.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(AlignmentSummaryGpuPageError::OverlappingDepthSpans(
                page.key(),
            ));
        }
        entries.shrink_to_fit();
        let leaf_base = if entries.is_empty() {
            0
        } else {
            entries.len().checked_next_power_of_two().unwrap_or(0)
        };
        let mut maximum_tree = if leaf_base == 0 {
            Vec::new()
        } else {
            vec![0_u32; leaf_base.saturating_mul(2)]
        };
        for (index, entry) in entries.iter().enumerate() {
            maximum_tree[leaf_base + index] = entry.depth;
        }
        for index in (1..leaf_base).rev() {
            maximum_tree[index] = maximum_tree[index * 2].max(maximum_tree[index * 2 + 1]);
        }
        maximum_tree.shrink_to_fit();
        Ok(Self {
            entries,
            maximum_tree,
            leaf_base,
        })
    }

    #[must_use]
    pub fn maximum_depth_exact(&self, exact_start: f64, exact_end: f64) -> u32 {
        if !exact_start.is_finite() || !exact_end.is_finite() || exact_start >= exact_end {
            return 0;
        }
        let first = self
            .entries
            .partition_point(|entry| entry.end as f64 <= exact_start);
        let last = self
            .entries
            .partition_point(|entry| (entry.start as f64) < exact_end);
        self.maximum_over_indices(first, last)
    }

    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.owned_heap_bytes())
    }

    fn owned_heap_bytes(&self) -> usize {
        self.entries
            .capacity()
            .saturating_mul(size_of::<DepthRangeEntry>())
            .saturating_add(
                self.maximum_tree
                    .capacity()
                    .saturating_mul(size_of::<u32>()),
            )
    }

    fn maximum_over_indices(&self, mut start: usize, mut end: usize) -> u32 {
        if start >= end || self.maximum_tree.is_empty() {
            return 0;
        }
        start = start.saturating_add(self.leaf_base);
        end = end.saturating_add(self.leaf_base);
        let mut maximum = 0;
        while start < end {
            if !start.is_multiple_of(2) {
                maximum = maximum.max(self.maximum_tree[start]);
                start = start.saturating_add(1);
            }
            if !end.is_multiple_of(2) {
                end = end.saturating_sub(1);
                maximum = maximum.max(self.maximum_tree[end]);
            }
            start /= 2;
            end /= 2;
        }
        maximum
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentSummaryModificationPageMetadata {
    background_range: Range<u32>,
    raw_range: Range<u32>,
    weighted_range: Range<u32>,
    codes: Vec<ModificationCode>,
}

impl AlignmentSummaryModificationPageMetadata {
    #[must_use]
    pub fn background_range(&self) -> Range<u32> {
        self.background_range.clone()
    }

    #[must_use]
    pub fn raw_range(&self) -> Range<u32> {
        self.raw_range.clone()
    }

    #[must_use]
    pub fn weighted_range(&self) -> Range<u32> {
        self.weighted_range.clone()
    }

    #[must_use]
    pub fn codes(&self) -> &[ModificationCode] {
        &self.codes
    }

    fn hidden_code_mask(&self, hidden: &[ModificationCode]) -> u32 {
        hidden.iter().fold(0_u32, |mask, code| {
            self.codes
                .binary_search(code)
                .ok()
                .and_then(|index| u32::try_from(index).ok())
                .filter(|index| *index < u32::BITS)
                .map_or(mask, |index| mask | (1_u32 << index))
        })
    }

    fn owned_heap_bytes(&self) -> usize {
        self.codes
            .capacity()
            .saturating_mul(size_of::<ModificationCode>())
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
            family => Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family)),
        }
    }

    #[must_use]
    pub fn instance_count(&self) -> usize {
        match self {
            Self::Depth { instances, .. } => instances.len(),
            Self::Alleles { instances } => instances.len(),
            Self::Modifications { instances, .. } => instances.len(),
        }
    }

    #[must_use]
    pub fn uploaded_bytes(&self) -> usize {
        match self {
            Self::Depth { instances, .. } => size_of_val(instances.as_slice()),
            Self::Alleles { instances } => size_of_val(instances.as_slice()),
            Self::Modifications { instances, .. } => size_of_val(instances.as_slice()),
        }
    }

    fn range_index_bytes(&self) -> usize {
        match self {
            // The index value itself lives inline in `GpuSummaryPageMetadata` and is charged by
            // the page-table allocation. Only its owned vectors belong to the per-page lease.
            Self::Depth { range_maximum, .. } => range_maximum.owned_heap_bytes(),
            Self::Alleles { .. } => 0,
            Self::Modifications { metadata, .. } => metadata.owned_heap_bytes(),
        }
    }
}

fn pack_depth_page(
    page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    let range_maximum = AlignmentSummaryDepthRangeIndex::build(page)?;
    let mut instances = Vec::with_capacity(page.primitives().len());
    for primitive in page.primitives() {
        let SummarySemanticComponent::Depth { depth, .. } = primitive.component else {
            return Err(AlignmentSummaryGpuPageError::UnexpectedSemanticComponent(
                page.key(),
            ));
        };
        instances.push(AlignmentSummaryGpuDepthInstance {
            start_offset: split_f64(primitive.start_offset as f64),
            end_offset: split_f64(primitive.end_offset as f64),
            depth,
            padding: [0; 3],
        });
    }
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Depth {
        instances,
        range_maximum,
    })
}

fn pack_allele_page(
    page: &SummarySemanticPage,
    depth_page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::alleles(page, depth_page)?;
    let depth = AlignmentSummaryDepthRangeIndex::build(depth_page)?;
    let mut instances = Vec::with_capacity(page.primitives().len().saturating_mul(4));
    for primitive in page.primitives() {
        if primitive.start_offset >= primitive.end_offset {
            return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                page: page.key(),
                start: primitive.start_offset,
                end: primitive.end_offset,
            });
        }
        let SummarySemanticComponent::Alleles {
            bin,
            reference_base: Some(reference_base),
        } = primitive.component
        else {
            return Err(AlignmentSummaryGpuPageError::MissingReferenceBase(
                page.key(),
            ));
        };
        let start = page
            .interval()
            .start()
            .saturating_add(primitive.start_offset);
        let end = page.interval().start().saturating_add(primitive.end_offset);
        let local_depth = depth.maximum_depth_exact(start as f64, end as f64);
        let raw_denominator = f64::from(bin.alleles.base_observations());
        let weighted_denominator = bin.quality_weighted.total() as f64;
        let bases = [
            (AlignmentSummaryAlleleToken::A, b'A'),
            (AlignmentSummaryAlleleToken::C, b'C'),
            (AlignmentSummaryAlleleToken::G, b'G'),
            (AlignmentSummaryAlleleToken::T, b'T'),
            (AlignmentSummaryAlleleToken::N, b'N'),
        ];
        let raw = bases.map(|(_, base)| {
            if base == reference_base.to_ascii_uppercase() {
                0.0
            } else {
                f64::from(bin.alleles.for_base(base))
            }
        });
        let weighted = bases.map(|(_, base)| {
            if base == reference_base.to_ascii_uppercase() {
                0.0
            } else {
                bin.quality_weighted.for_base(base) as f64
            }
        });
        let raw_alternate = raw.iter().sum::<f64>();
        let weighted_alternate = weighted.iter().sum::<f64>();
        let mut raw_before = 0.0;
        let mut weighted_before = 0.0;
        for (index, (allele, base)) in bases.into_iter().enumerate() {
            if base == reference_base.to_ascii_uppercase() {
                continue;
            }
            let raw_fraction = safe_fraction(raw[index], raw_denominator);
            let weighted_fraction = safe_fraction(weighted[index], weighted_denominator);
            if raw_fraction == 0.0 && weighted_fraction == 0.0 {
                continue;
            }
            instances.push(AlignmentSummaryGpuAlleleInstance {
                start_offset: split_f64(primitive.start_offset as f64),
                end_offset: split_f64(primitive.end_offset as f64),
                local_depth,
                allele: allele as u32,
                raw: [
                    raw_fraction as f32,
                    safe_fraction(raw_alternate, raw_denominator) as f32,
                    raw_before as f32,
                ],
                quality_weighted: [
                    weighted_fraction as f32,
                    safe_fraction(weighted_alternate, weighted_denominator) as f32,
                    weighted_before as f32,
                ],
            });
            raw_before += raw_fraction;
            weighted_before += weighted_fraction;
        }
    }
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Alleles { instances })
}

#[derive(Clone, Copy)]
struct ModificationPackingCandidate {
    start_offset: u64,
    end_offset: u64,
    series: ModificationSeriesKey,
    bin: ModificationBin,
    raw_fraction: f64,
    weighted_fraction: f64,
    code_index: u32,
    by_type_color: [f32; 4],
}

fn pack_modification_page(
    page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::modifications(page)?;
    let mut semantic = Vec::with_capacity(page.primitives().len());
    let mut codes = BTreeSet::new();
    for primitive in page.primitives() {
        if primitive.start_offset >= primitive.end_offset {
            return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                page: page.key(),
                start: primitive.start_offset,
                end: primitive.end_offset,
            });
        }
        let SummarySemanticComponent::Modification { series, bin } = primitive.component else {
            return Err(AlignmentSummaryGpuPageError::UnexpectedSemanticComponent(
                page.key(),
            ));
        };
        if bin.modified_calls == 0 {
            continue;
        }
        let (Some(raw_fraction), Some(weighted_fraction)) =
            (bin.raw_fraction(), bin.weighted_fraction())
        else {
            continue;
        };
        codes.insert(series.code);
        semantic.push((
            primitive.start_offset,
            primitive.end_offset,
            series,
            bin,
            raw_fraction.clamp(0.0, 1.0),
            weighted_fraction.clamp(0.0, 1.0),
        ));
    }
    let mut codes = codes.into_iter().collect::<Vec<_>>();
    if codes.len() > u32::BITS as usize {
        return Err(AlignmentSummaryGpuPageError::TooManyModificationCodes(
            codes.len(),
        ));
    }
    codes.shrink_to_fit();
    let mut groups = BTreeMap::<(u64, u64), Vec<ModificationPackingCandidate>>::new();
    for (start_offset, end_offset, series, bin, raw_fraction, weighted_fraction) in semantic {
        let code_index = u32::try_from(
            codes
                .binary_search(&series.code)
                .expect("semantic modification code belongs to the page dictionary"),
        )
        .expect("page modification code dictionary is bounded to u32 bits");
        let color = modification_code_color(series.code);
        groups
            .entry((start_offset, end_offset))
            .or_default()
            .push(ModificationPackingCandidate {
                start_offset,
                end_offset,
                series,
                bin,
                raw_fraction,
                weighted_fraction,
                code_index,
                by_type_color: [color.red, color.green, color.blue, color.alpha],
            });
    }

    let maximum_instances = groups
        .values()
        .try_fold(groups.len(), |total, group| {
            total.checked_add(group.len().saturating_mul(2))
        })
        .ok_or(AlignmentSummaryGpuPageError::CapacityOverflow)?;
    let mut instances = Vec::with_capacity(maximum_instances);
    for (&(start_offset, end_offset), candidates) in &groups {
        let modified_code_mask = candidates.iter().fold(0_u32, |mask, candidate| {
            mask | (1_u32 << candidate.code_index)
        });
        instances.push(AlignmentSummaryGpuModificationInstance {
            start_offset: split_f64(start_offset as f64),
            end_offset: split_f64(end_offset as f64),
            fractions: [0.0; 2],
            by_type_color: [0.14, 0.44, 0.82, 0.20],
            code_index: 0,
            later_code_mask: modified_code_mask,
            modified_calls: 0,
            canonical_observations: 0,
            shape: 1,
            padding: [0; 3],
        });
    }
    let background_end = u32::try_from(instances.len())
        .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;

    let raw_start = background_end;
    append_sorted_modification_candidates(&mut instances, &groups, false)?;
    let raw_end = u32::try_from(instances.len())
        .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
    let weighted_start = raw_end;
    append_sorted_modification_candidates(&mut instances, &groups, true)?;
    let weighted_end = u32::try_from(instances.len())
        .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Modifications {
        instances,
        metadata: AlignmentSummaryModificationPageMetadata {
            background_range: 0..background_end,
            raw_range: raw_start..raw_end,
            weighted_range: weighted_start..weighted_end,
            codes,
        },
    })
}

fn append_sorted_modification_candidates(
    instances: &mut Vec<AlignmentSummaryGpuModificationInstance>,
    groups: &BTreeMap<(u64, u64), Vec<ModificationPackingCandidate>>,
    probability_weighted: bool,
) -> Result<(), AlignmentSummaryGpuPageError> {
    for candidates in groups.values() {
        let mut candidates = candidates.clone();
        candidates.sort_by(|left, right| {
            let left_fraction = if probability_weighted {
                left.weighted_fraction
            } else {
                left.raw_fraction
            };
            let right_fraction = if probability_weighted {
                right.weighted_fraction
            } else {
                right.raw_fraction
            };
            left_fraction
                .total_cmp(&right_fraction)
                .then_with(|| right.series.cmp(&left.series))
        });
        for (index, candidate) in candidates.iter().enumerate() {
            let later_code_mask = candidates[index.saturating_add(1)..]
                .iter()
                .fold(0_u32, |mask, later| mask | (1_u32 << later.code_index));
            instances.push(AlignmentSummaryGpuModificationInstance {
                start_offset: split_f64(candidate.start_offset as f64),
                end_offset: split_f64(candidate.end_offset as f64),
                fractions: [
                    candidate.raw_fraction as f32,
                    candidate.weighted_fraction as f32,
                ],
                by_type_color: candidate.by_type_color,
                code_index: candidate.code_index,
                later_code_mask,
                modified_calls: candidate.bin.modified_calls,
                canonical_observations: candidate.bin.canonical_observations,
                shape: 0,
                padding: [0; 3],
            });
        }
    }
    if instances.len() > u32::MAX as usize {
        return Err(AlignmentSummaryGpuPageError::CapacityOverflow);
    }
    Ok(())
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
}

/// Typed renderer-facing lane contract. Camera-independent semantic page identity is selected by
/// the variant; live filtering, weighting, colouring, and marker state stay in the frame request.
#[derive(Clone, Debug, PartialEq)]
pub enum AlignmentSummaryGpuLaneOptions {
    Coverage(AlignmentCoverageLaneOptions),
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

pub struct AlignmentSummaryGpuSceneRequest<'a> {
    pub scene: &'a Arc<AlignmentSceneSnapshot>,
    pub camera_sequence: u64,
    pub camera: CpuSummaryCamera,
    pub screen_size_points: [f32; 2],
    pub scissor: AlignmentGpuScissor,
    pub options: AlignmentSummaryGpuLaneOptions,
    pub overlays: AlignmentSummaryFrameOverlays,
}

fn summary_content_selectors(
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
) -> Vec<AlignmentSummaryGpuContentSelector> {
    requests
        .iter()
        .map(|request| match &request.options {
            AlignmentSummaryGpuLaneOptions::Coverage(options) => {
                AlignmentSummaryGpuContentSelector::Coverage {
                    allele_interpretation: options.alleles.map(|alleles| alleles.interpretation),
                }
            }
            AlignmentSummaryGpuLaneOptions::Modifications(_) => {
                AlignmentSummaryGpuContentSelector::Modifications
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
    page_start_from_camera: [f32; 2],
    covered_clip_x: [f32; 2],
    scale_maximum: f32,
    minimum_alternate_fraction: f32,
    transform: u32,
    quality_weighted: u32,
    family: u32,
    hidden_modification_mask: u32,
    modification_color_mode: u32,
    padding: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSummaryGpuPrepareStats {
    pub scenes: usize,
    pub mandatory_pages: usize,
    pub depth_instances: usize,
    pub allele_instances: usize,
    pub modification_instances: usize,
    pub overlay_instances: usize,
    pub camera_uniform_bytes: usize,
    pub geometry_uploaded_bytes: usize,
    pub range_maximum_queries: usize,
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
    )>,
    next_page: usize,
    pins: Vec<PinnedAllocationLease>,
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

#[derive(Clone, Copy, Debug)]
struct SummaryPageReclaimCandidate {
    key: AlignmentSummaryGpuPageKey,
}

#[derive(Clone, Debug)]
struct PreparedSummaryDraw {
    page: AlignmentSummaryGpuPageKey,
    uniform_offset: u32,
    scissor: AlignmentGpuScissor,
    background_range: Range<u32>,
    foreground_range: Range<u32>,
}

#[derive(Clone, Debug)]
struct PendingSummaryDraw {
    page: AlignmentSummaryGpuPageKey,
    camera: CpuSummaryCamera,
    screen_size: [f32; 2],
    scissor: AlignmentGpuScissor,
    page_start: u64,
    covered_clip_x: [f32; 2],
    scale_maximum: u32,
    minimum_alternate_fraction: f32,
    transform: CoverageScaleTransform,
    quality_weighted: bool,
    hidden_modification_mask: u32,
    modification_color_mode: ModifiedBaseColorMode,
    background_range: Range<u32>,
    foreground_range: Range<u32>,
}

#[derive(Clone, Debug)]
struct SummaryDrawStyle {
    minimum_alternate_fraction: f32,
    transform: CoverageScaleTransform,
    quality_weighted: bool,
    hidden_modification_mask: u32,
    modification_color_mode: ModifiedBaseColorMode,
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
    Coverage {
        allele_interpretation: Option<CoverageAlleleInterpretation>,
    },
    Modifications,
}

/// Camera-independent content authority for an exact-summary presentation. Replacement
/// publication mints the ordered immutable scene identities, lane content selectors, and complete
/// prepared-page manifest once. Cameras, scale transforms, thresholds, hidden modification codes,
/// colours, and overlays stay outside this token and may therefore change through the no-bundle
/// capture path.
///
/// Camera capture validates only the constant-size-per-lane identities/selectors. The manifest
/// remains exact replacement/retirement authority but is not rebuilt during navigation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSummaryGpuContentAuthority {
    device_generation: u64,
    scene_revisions: Arc<[SceneRevision]>,
    scene_allocations: Arc<[AllocationId]>,
    content_selectors: Arc<[AlignmentSummaryGpuContentSelector]>,
    page_manifest: Arc<[AlignmentSummaryGpuPageKey]>,
}

impl AlignmentSummaryGpuContentAuthority {
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
    modification_instances: usize,
    camera_uniform_bytes: usize,
    geometry_uploaded_bytes: usize,
    range_maximum_queries: usize,
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
    modification_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    uniform_stride: usize,
    frame_buffers: Vec<Arc<GpuSummaryFrameBuffers>>,
    pages: Vec<GpuSummaryPage>,
    /// Incrementally maintained LRU authority used to produce bounded pressure-time reclaim
    /// results without scanning and sorting the complete resident summary page table.
    reclaim_index: BTreeSet<(u64, AlignmentSummaryGpuPageKey)>,
    page_table_allocation: Option<AllocationLease>,
    page_table_capacity: usize,
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
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RGV alignment summary semantic page shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
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
            bind_group_layouts: &[Some(&uniform_layout)],
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
            frame_buffers: Vec::new(),
            pages: Vec::new(),
            reclaim_index: BTreeSet::new(),
            page_table_allocation: None,
            page_table_capacity: 0,
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
            .pages
            .len()
            .checked_add(missing_pages.len())
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
        let (page_table_slot, page_table_requested_capacity, page_table_maximum_bytes) =
            if required_page_capacity > self.page_table_capacity {
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
        let overlay_instance_envelope = requests
            .len()
            .checked_mul(2)
            .ok_or(AlignmentSummaryGpuReplacementError::CapacityOverflow)?;
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
        for frame in prior_frames.iter() {
            for pin in &frame._pins {
                *prior_pin_counts.entry(pin.id()).or_default() += 1;
            }
        }
        let prepared = plan
            .prepared_page_keys
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut all_pages_retirable = true;
        for page in &self.pages {
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
                .page_table_allocation
                .as_ref()
                .map(|allocation| exact_summary_retirement_ownership(allocation, &prior_pin_counts))
                .transpose()?
                .unwrap_or(false);

        let mut claimable = BTreeSet::<AllocationId>::new();
        let mut retired_page_indices = Vec::new();
        if can_retire_table {
            for page in &self.pages {
                if let Some(allocation) = &page.allocation {
                    claimable.insert(allocation.id());
                }
            }
        } else {
            for (index, page) in self.pages.iter().enumerate() {
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
            if let Some(allocation) = &self.page_table_allocation {
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
        for frame in prior_frames.iter() {
            for pin in &frame._pins {
                if claimable.contains(&pin.id()) {
                    builder.include_pin(pin)?;
                }
            }
        }
        if can_retire_table {
            for page in &self.pages {
                if let Some(allocation) = &page.allocation {
                    builder.include_lease(allocation)?;
                }
            }
        } else {
            for index in &retired_page_indices {
                if let Some(allocation) = &self.pages[*index].allocation {
                    builder.include_lease(allocation)?;
                }
            }
        }
        if can_retire_table && let Some(allocation) = &self.page_table_allocation {
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
            std::mem::take(&mut self.pages)
        } else {
            let mut retired = Vec::with_capacity(retired_page_indices.len());
            for index in retired_page_indices.into_iter().rev() {
                retired.push(self.pages.remove(index));
            }
            retired.sort_unstable_by_key(|page| page.key);
            retired
        };
        let retired_page_table = if can_retire_table {
            let capacity = self.page_table_capacity;
            self.page_table_capacity = 0;
            self.page_table_allocation
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
            && (self.page_table_allocation.is_some() || !self.pages.is_empty())
        {
            return Err(AlignmentSummaryGpuReplacementError::RetirementRestoreConflict);
        }
        if let Some((allocation, capacity)) = retirement.retired_page_table.take() {
            if !self.pages.is_empty() {
                return Err(AlignmentSummaryGpuReplacementError::RetirementRestoreConflict);
            }
            // The token owns the original Vec allocation as well as its ledger lease. Move that
            // exact table back instead of copying entries into a different backing allocation.
            self.pages = std::mem::take(&mut retirement.retired_pages);
            self.page_table_allocation = Some(allocation);
            self.page_table_capacity = capacity;
        } else {
            self.pages.append(&mut retirement.retired_pages);
            self.pages.sort_unstable_by_key(|page| page.key);
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
            let required = self.pages.len().saturating_add(staged_pages.len());
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

        let mut pending_draws = Vec::with_capacity(plan.mandatory_draws);
        let mut overlays = Vec::with_capacity(plan.overlay_instances);
        let mut pending_overlay_draws = Vec::with_capacity(plan.overlay_draws);
        let scene_revisions = plan.scene_revisions.to_vec();
        let scene_allocations = plan.scene_allocations.to_vec();
        let mut camera_sequences = Vec::with_capacity(requests.len());
        let mut depth_instances = 0_usize;
        let mut allele_instances = 0_usize;
        let mut modification_instances = 0_usize;
        for (((request, pages), covered), scale_maximum) in requests
            .iter()
            .zip(&visible_pages)
            .zip(&covered)
            .zip(resolved_depth_scales.iter().copied())
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
                    family => {
                        return Err(AlignmentSummaryGpuPageError::UnsupportedFamily(family).into());
                    }
                }
                pending_draws.push(PendingSummaryDraw {
                    page: page_request.key,
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
                    hidden_modification_mask: style.hidden_modification_mask,
                    modification_color_mode: style.modification_color_mode,
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
                request.page.family(),
                request.hidden_modification_mask,
                request.modification_color_mode,
            );
            let offset = uniform_index.saturating_mul(self.uniform_stride);
            uniforms[offset..offset + size_of::<AlignmentSummaryGpuUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
            draws.push(PreparedSummaryDraw {
                page: request.page,
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
                SummaryPageFamily::Depth,
                0,
                ModifiedBaseColorMode::ByType,
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
            && let Some(allocation) = &self.page_table_allocation
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
            modification_instances,
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes,
            range_maximum_queries,
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
            page_table.extend(std::mem::take(&mut self.pages));
            page_table.append(&mut new_pages);
            page_table.sort_unstable_by_key(|page| page.key);
            self.pages = page_table;
            self.page_table_capacity = self.pages.capacity();
            self.page_table_allocation = new_page_table_allocation;
        } else {
            debug_assert!(new_page_table_allocation.is_none());
            self.pages.append(&mut new_pages);
            self.pages.sort_unstable_by_key(|page| page.key);
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
        let content_authority = AlignmentSummaryGpuContentAuthority {
            device_generation: self.device_generation,
            scene_revisions: staged.scene_revisions.clone().into(),
            scene_allocations: staged.scene_allocations.into(),
            content_selectors: staged.plan.content_selectors.clone().into(),
            page_manifest: Arc::clone(&staged.plan.prepared_page_keys),
        };
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
            modification_instances: staged.modification_instances,
            overlay_instances: staged.plan.overlay_instances,
            camera_uniform_bytes: staged.camera_uniform_bytes,
            geometry_uploaded_bytes: std::mem::take(&mut self.pending_geometry_uploaded_bytes),
            range_maximum_queries: staged.range_maximum_queries,
            page_cache_hits: staged.plan.page_cache_hits,
            page_cache_misses: staged.plan.page_cache_misses,
            resident_pages: self.pages.len(),
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
            self.pages.len().saturating_add(pending.len()),
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
                self.pages[index].last_used_revision = revision;
            }
        }
        self.pages.extend(admitted);
        self.pages.sort_unstable_by_key(|page| page.key);
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
        self.prepare_internal(device, queue, requests, Some(ledger), None, true)
    }

    fn prepare_internal(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: Option<&AllocationLedger>,
        existing_content_authority: Option<&AlignmentSummaryGpuContentAuthority>,
        allow_slot_growth: bool,
    ) -> Result<
        (AlignmentSummaryGpuFrame, AlignmentSummaryGpuPrepareStats),
        AlignmentSummaryGpuPrepareError,
    > {
        for request in requests {
            validate_summary_request(request)?;
        }
        let prepared_page_keys = existing_content_authority
            .is_none()
            .then(|| Self::prepared_page_keys(requests))
            .transpose()?;
        if let Some(prepared_page_keys) = &prepared_page_keys {
            self.record_content_manifest_build(prepared_page_keys.len());
        }
        let prepared_page_count = existing_content_authority.map_or_else(
            || {
                prepared_page_keys
                    .as_ref()
                    .expect("new summary content authority requires a prepared-page manifest")
                    .len()
            },
            |authority| authority.page_manifest.len(),
        );
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
        let _scratch = ledger
            .map(|ledger| {
                ledger.reserve_transient(
                    TransientResourceClass::GpuUpload,
                    scratch_bytes,
                    ReservationKind::Transition,
                )
            })
            .transpose()?;
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
                ledger.expect("growing a summary frame slot requires a shared ledger"),
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
        let mut scene_allocations = Vec::with_capacity(requests.len());
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for (((request, pages), covered), scale_maximum) in requests
            .iter()
            .zip(&visible)
            .zip(&covered)
            .zip(resolved_depth_scales.iter().copied())
        {
            scene_revisions.push(request.scene.revision());
            scene_allocations.push(request.scene.metadata_allocation().id());
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
                    hidden_modification_mask: style.hidden_modification_mask,
                    modification_color_mode: style.modification_color_mode,
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
        let mut pins = Vec::with_capacity(mandatory_pages.saturating_add(4));
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
                request.page.family(),
                request.hidden_modification_mask,
                request.modification_color_mode,
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
            let page = &self.pages[page_index];
            if let Some(allocation) = &page.allocation {
                pins.push(allocation.pin()?);
            }
            draws.push(PreparedSummaryDraw {
                page: request.page,
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
                SummaryPageFamily::Depth,
                0,
                ModifiedBaseColorMode::ByType,
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
        if let Some(allocation) = &self.page_table_allocation {
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
            pins.len().saturating_add(1),
        );
        if retained_frame_bytes > frame_buffers.frame_capacity {
            return Err(AlignmentSummaryGpuPrepareError::FrameSlotCapacity {
                required: retained_frame_bytes,
                available: frame_buffers.frame_capacity,
            });
        }
        pins.push(frame_buffers.frame_allocation.pin()?);
        let content_authority = existing_content_authority.cloned().unwrap_or_else(|| {
            AlignmentSummaryGpuContentAuthority {
                device_generation: self.device_generation,
                scene_revisions: scene_revisions.clone().into(),
                scene_allocations: scene_allocations.into(),
                content_selectors: summary_content_selectors(requests).into(),
                page_manifest: prepared_page_keys
                    .expect("new summary content authority requires a prepared-page manifest")
                    .into(),
            }
        });
        let frame = AlignmentSummaryGpuFrame {
            revision: next_revision,
            device_generation: self.device_generation,
            scene_revisions: scene_revisions.into(),
            camera_sequences: camera_sequences.into(),
            resolved_depth_scales: resolved_depth_scales.into(),
            content_authority,
            draws: draws.into(),
            overlay_draws: prepared_overlays.into(),
            buffers: frame_buffers,
            _pins: pins,
        };
        let resident_gpu_bytes = self.resident_gpu_bytes()?;
        let stats = AlignmentSummaryGpuPrepareStats {
            scenes: requests.len(),
            mandatory_pages,
            depth_instances,
            allele_instances,
            modification_instances,
            overlay_instances: overlays.len(),
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes: std::mem::take(&mut self.pending_geometry_uploaded_bytes),
            range_maximum_queries,
            page_cache_hits,
            page_cache_misses: 0,
            resident_pages: self.pages.len(),
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
        let result = self.prepare_internal(
            device,
            queue,
            requests,
            Some(ledger),
            Some(authority),
            false,
        );
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
        })
    }

    /// Starts a summary warm-up in O(scene lanes). Each later advance resolves only its bounded
    /// slice of the replacement-minted authority.
    pub fn begin_prepared_scene_prefetch_cursor(
        &self,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        page_keys: Arc<[AlignmentSummaryGpuPageKey]>,
    ) -> Result<AlignmentSummaryGpuPrefetchCursor, AlignmentSummaryGpuPrefetchCursorError> {
        let selectors = summary_content_selectors(requests);
        let scenes = requests
            .iter()
            .zip(selectors)
            .map(|(request, selector)| (Arc::clone(request.scene), selector))
            .collect();
        Ok(AlignmentSummaryGpuPrefetchCursor {
            device_generation: self.device_generation,
            page_keys,
            scenes,
            next_page: 0,
            pins: Vec::new(),
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
        for key in cursor.page_keys[cursor.next_page..end].iter().copied() {
            let page = summary_cursor_page(&cursor.scenes, key)?;
            if let Some(resident) = self.page(key) {
                if let Some(allocation) = &resident.allocation {
                    resident_pins.push(allocation.pin()?);
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
                cursor.pins.push(allocation.pin()?);
            }
        }
        cursor.pins.extend(resident_pins);
        cursor.next_page = end;
        let remaining_authority_pages = cursor.remaining_pages();
        Ok(AlignmentSummaryGpuPrefetchProgress {
            prepared_pages: cursor.page_keys.len(),
            missing_pages_before: authority_pages_before,
            admission,
            remaining_missing_pages: remaining_authority_pages,
            missing_pages_remain: remaining_authority_pages > 0,
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
        for draw in frame.draws.iter() {
            let page = self
                .page(draw.page)
                .ok_or(AlignmentSummaryGpuRenderError::MissingPinnedPage(draw.page))?;
            pass.set_pipeline(match draw.page.family() {
                SummaryPageFamily::Depth => &self.depth_pipeline,
                SummaryPageFamily::Alleles => &self.allele_pipeline,
                SummaryPageFamily::Modifications => &self.modification_pipeline,
                family => {
                    return Err(AlignmentSummaryGpuRenderError::UnsupportedFamily(family));
                }
            });
            pass.set_scissor_rect(
                draw.scissor.x,
                draw.scissor.y,
                draw.scissor.width,
                draw.scissor.height,
            );
            pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
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
            pass.set_vertex_buffer(0, overlay.slice(..));
            for draw in frame.overlay_draws.iter() {
                pass.set_scissor_rect(
                    draw.scissor.x,
                    draw.scissor.y,
                    draw.scissor.width,
                    draw.scissor.height,
                );
                pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
                pass.draw(0..6, draw.range.clone());
            }
        }
        Ok(())
    }

    /// Drops every device-derived allocation while preserving all scene-owned semantic pages.
    pub fn discard_device_resources(&mut self) {
        self.pages = Vec::new();
        self.reclaim_index.clear();
        self.page_table_allocation = None;
        self.page_table_capacity = 0;
        self.frame_buffers.clear();
        self.pending_geometry_uploaded_bytes = 0;
        self.device_generation = self.device_generation.saturating_add(1).max(1);
    }

    #[must_use]
    pub fn resident_page_count(&self) -> usize {
        self.pages.len()
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
        let ledger_before = ledger.progress()?;
        let pages_before = self.pages.len();
        let retained_bytes_before = self.resident_retained_bytes()?;
        if goal.is_feasible() && !shared_ledger_satisfies_goal(&ledger_before, goal) {
            self.reclaim_all_idle_frame_buffers();
            self.reclaim_sparse_page_table(ledger)?;
            loop {
                let snapshot = ledger.progress()?;
                if shared_ledger_satisfies_goal(&snapshot, goal) {
                    break;
                }
                let occupied = snapshot.charged.saturating_add(snapshot.reserved);
                let candidates = self
                    .indexed_unpinned_lru_candidates_until(protected_pages, |released| {
                        goal.is_satisfied_at(occupied.saturating_sub(released))
                    })?;
                if candidates.is_empty() {
                    break;
                }
                let checkpoint = self
                    .removals_until_sparse_page_table_checkpoint()?
                    .unwrap_or(candidates.len());
                let remove = checkpoint.min(candidates.len());
                if remove == 0 {
                    break;
                }
                self.remove_reclaim_candidates(&candidates[..remove]);
                self.reclaim_sparse_page_table(ledger)?;
            }
        }
        let retained_bytes_after = self.resident_retained_bytes()?;
        let ledger_after = ledger.progress()?;
        Ok(GpuReclaimReport {
            pressure: goal.pressure,
            baseline_occupied_bytes: goal.baseline_occupied_bytes,
            pages_before,
            pages_after: self.pages.len(),
            retained_bytes_before,
            retained_bytes_after,
            ledger_charged_before: ledger_before.charged,
            ledger_charged_after: ledger_after.charged,
            headroom_satisfied: shared_ledger_satisfies_goal(&ledger_after, goal),
        })
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
        let mut total = self.resident_gpu_bytes()?;
        for slot in &self.frame_buffers {
            total = total.saturating_add(slot.frame_allocation.snapshot()?.charged_capacity);
        }
        Ok(total)
    }

    fn resident_gpu_bytes(&self) -> Result<usize, LedgerError> {
        let mut total = 0_usize;
        for page in &self.pages {
            if let Some(allocation) = &page.allocation {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
        }
        if let Some(allocation) = &self.page_table_allocation {
            total = total.saturating_add(allocation.snapshot()?.charged_capacity);
        }
        for slot in &self.frame_buffers {
            if let Some(allocation) = &slot.uniform_allocation {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
            if let Some(allocation) = &slot.overlay_allocation {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
        }
        Ok(total)
    }

    fn page_index(&self, key: AlignmentSummaryGpuPageKey) -> Option<usize> {
        self.pages
            .binary_search_by_key(&key, |resident| resident.key)
            .ok()
    }

    fn page(&self, key: AlignmentSummaryGpuPageKey) -> Option<&GpuSummaryPage> {
        self.page_index(key).map(|index| &self.pages[index])
    }

    fn page_mut(&mut self, key: AlignmentSummaryGpuPageKey) -> Option<&mut GpuSummaryPage> {
        self.page_index(key).map(|index| &mut self.pages[index])
    }

    fn rebuild_reclaim_index(&mut self) {
        self.reclaim_index.clear();
        self.reclaim_index.extend(
            self.pages
                .iter()
                .map(|page| (page.last_used_revision, page.key)),
        );
        debug_assert_eq!(self.reclaim_index.len(), self.pages.len());
    }

    fn touch_page_at(&mut self, index: usize, revision: u64) {
        let page = &mut self.pages[index];
        self.reclaim_index
            .remove(&(page.last_used_revision, page.key));
        page.last_used_revision = revision;
        assert!(self.reclaim_index.insert((revision, page.key)));
    }

    fn ensure_page_table_capacity(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        reservation_kind: ReservationKind,
    ) -> Result<(), AlignmentSummaryGpuPageAdmissionError> {
        if required <= self.page_table_capacity {
            return Ok(());
        }
        let requested_capacity = next_summary_capacity(required)?;
        let maximum_bytes = requested_capacity
            .checked_mul(2)
            .and_then(summary_page_table_retained_bytes)
            .ok_or(AlignmentSummaryGpuPageAdmissionError::CapacityOverflow)?;
        let mut reservation =
            ledger.reserve(AllocationClass::GpuPage, maximum_bytes, reservation_kind)?;
        let mut candidate = Vec::with_capacity(requested_capacity);
        let actual_capacity = candidate.capacity();
        if actual_capacity < required {
            return Err(AlignmentSummaryGpuPageAdmissionError::PageTableCapacity {
                required,
                actual: actual_capacity,
            });
        }
        let actual_bytes = summary_page_table_retained_bytes(actual_capacity)
            .ok_or(AlignmentSummaryGpuPageAdmissionError::CapacityOverflow)?;
        if actual_bytes > maximum_bytes {
            return Err(AlignmentSummaryGpuPageAdmissionError::PageTableCapacity {
                required: maximum_bytes,
                actual: actual_bytes,
            });
        }
        let allocation = reservation.register(actual_bytes, self.revision)?;
        candidate.extend(std::mem::take(&mut self.pages));
        self.pages = candidate;
        self.page_table_capacity = actual_capacity;
        self.page_table_allocation = Some(allocation);
        Ok(())
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
        self.frame_buffers
            .retain(|slot| slot.capture_ring || Arc::strong_count(slot) > 1);
    }

    fn reclaim_all_idle_frame_buffers(&mut self) {
        self.frame_buffers
            .retain(|slot| Arc::strong_count(slot) > 1);
    }

    fn reclaim_sparse_page_table(&mut self, ledger: &AllocationLedger) -> Result<(), LedgerError> {
        let Some(allocation) = self.page_table_allocation.as_ref() else {
            return Ok(());
        };
        if allocation.snapshot()?.pins > 0 {
            return Ok(());
        }
        if self.pages.is_empty() {
            self.pages = Vec::new();
            self.page_table_allocation = None;
            self.page_table_capacity = 0;
            return Ok(());
        }
        let Some(requested_capacity) = self.pages.len().checked_next_power_of_two() else {
            return Ok(());
        };
        if requested_capacity.saturating_mul(4) > self.page_table_capacity {
            return Ok(());
        }
        let maximum_bytes = summary_page_table_retained_bytes(requested_capacity.saturating_mul(2))
            .unwrap_or(usize::MAX);
        let Ok(mut reservation) = ledger.reserve(
            AllocationClass::GpuPage,
            maximum_bytes,
            ReservationKind::Transition,
        ) else {
            return Ok(());
        };
        let mut candidate = Vec::with_capacity(requested_capacity);
        if candidate.capacity() < self.pages.len() {
            return Ok(());
        }
        let actual_bytes =
            summary_page_table_retained_bytes(candidate.capacity()).unwrap_or(usize::MAX);
        let Ok(allocation) = reservation.register(actual_bytes, self.revision) else {
            return Ok(());
        };
        candidate.extend(std::mem::take(&mut self.pages));
        self.pages = candidate;
        self.page_table_capacity = self.pages.capacity();
        self.page_table_allocation = Some(allocation);
        Ok(())
    }

    fn indexed_unpinned_lru_candidates_until(
        &self,
        protected_pages: &[AlignmentSummaryGpuPageKey],
        mut sufficient: impl FnMut(usize) -> bool,
    ) -> Result<Vec<SummaryPageReclaimCandidate>, LedgerError> {
        let mut protected = protected_pages.to_vec();
        protected.sort_unstable();
        protected.dedup();
        let mut candidates = Vec::new();
        let mut reclaimable_bytes = 0_usize;
        for (_last_used_revision, key) in &self.reclaim_index {
            self.reclaim_authority_keys_inspected
                .fetch_add(1, Ordering::Relaxed);
            if protected.binary_search(key).is_ok() {
                continue;
            }
            let page = self
                .page(*key)
                .expect("online summary reclaim authority contains only resident pages");
            let Some(allocation) = &page.allocation else {
                continue;
            };
            let allocation = allocation.snapshot()?;
            if allocation.pins == 0 {
                candidates.push(SummaryPageReclaimCandidate { key: page.key });
                reclaimable_bytes = reclaimable_bytes.saturating_add(allocation.charged_capacity);
                if sufficient(reclaimable_bytes) {
                    break;
                }
            }
        }
        Ok(candidates)
    }

    fn removals_until_sparse_page_table_checkpoint(&self) -> Result<Option<usize>, LedgerError> {
        let Some(allocation) = self.page_table_allocation.as_ref() else {
            return Ok(None);
        };
        if allocation.snapshot()?.pins > 0 || self.pages.is_empty() {
            return Ok(None);
        }
        let quarter_capacity = self.page_table_capacity / 4;
        if quarter_capacity == 0 {
            return Ok(Some(self.pages.len()));
        }
        let maximum_sparse_len = 1_usize
            .checked_shl(usize::BITS - quarter_capacity.leading_zeros() - 1)
            .unwrap_or(0);
        if self.pages.len() <= maximum_sparse_len {
            // The immediately preceding shrink attempt was already eligible but could not
            // reserve its replacement. Retry after one more page releases capacity; combining
            // more removals would be able to overshoot a goal that the table release satisfies.
            return Ok(Some(1));
        }
        Ok(Some(self.pages.len().saturating_sub(maximum_sparse_len)))
    }

    fn remove_reclaim_candidates(&mut self, candidates: &[SummaryPageReclaimCandidate]) {
        let mut keys = candidates
            .iter()
            .map(|candidate| candidate.key)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        let mut next_key = 0;
        self.pages.retain(|page| {
            while keys
                .get(next_key)
                .is_some_and(|candidate| *candidate < page.key)
            {
                next_key = next_key.saturating_add(1);
            }
            if keys.get(next_key).is_some_and(|key| *key == page.key) {
                next_key = next_key.saturating_add(1);
                false
            } else {
                true
            }
        });
        self.rebuild_reclaim_index();
    }

    fn evict_unpinned_pages_for_excluding(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        limit: usize,
        protected: &[AlignmentSummaryGpuPageKey],
    ) -> Result<(), LedgerError> {
        self.reclaim_idle_frame_buffers();
        self.reclaim_sparse_page_table(ledger)?;
        loop {
            let snapshot = ledger.progress()?;
            if snapshot
                .charged
                .saturating_add(snapshot.reserved)
                .saturating_add(required)
                <= limit
            {
                return Ok(());
            }
            let occupied = snapshot.charged.saturating_add(snapshot.reserved);
            let candidates = self.indexed_unpinned_lru_candidates_until(protected, |released| {
                occupied.saturating_sub(released).saturating_add(required) <= limit
            })?;
            if candidates.is_empty() {
                return Ok(());
            }
            let checkpoint = self
                .removals_until_sparse_page_table_checkpoint()?
                .unwrap_or(candidates.len());
            let remove = checkpoint.min(candidates.len());
            if remove == 0 {
                return Ok(());
            }
            self.remove_reclaim_candidates(&candidates[..remove]);
            self.reclaim_sparse_page_table(ledger)?;
        }
    }
}

#[derive(Clone, Debug)]
struct OwnedSummaryPageRequest {
    key: AlignmentSummaryGpuPageKey,
    page: Arc<SummarySemanticPage>,
    companion_depth: Option<Arc<SummarySemanticPage>>,
}

impl OwnedSummaryPageRequest {
    fn borrowed(&self) -> AlignmentSummaryGpuPageRequest<'_> {
        AlignmentSummaryGpuPageRequest {
            page: &self.page,
            companion_depth: self.companion_depth.as_ref(),
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
    if let Some(alleles) = options.alleles
        && !matches!(
            alleles.interpretation,
            CoverageAlleleInterpretation::GenericReferenceAware
        )
    {
        return Err(
            AlignmentSummaryGpuPageError::UnsupportedAlleleInterpretation(alleles.interpretation),
        );
    }
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
    )],
    key: AlignmentSummaryGpuPageKey,
) -> Result<OwnedSummaryPageRequest, AlignmentSummaryGpuPrefetchCursorError> {
    for (scene, selector) in scenes {
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
                AlignmentSummaryGpuContentSelector::Coverage {
                    allele_interpretation: Some(CoverageAlleleInterpretation::GenericReferenceAware),
                },
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
            _ => continue,
        };
        let candidate = OwnedSummaryPageRequest {
            key,
            page: Arc::clone(page),
            companion_depth,
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
        PackedAlignmentSummaryPage::Depth { .. }
        | PackedAlignmentSummaryPage::Alleles { .. }
        | PackedAlignmentSummaryPage::Modifications { .. } => {}
    }
}

fn summary_gpu_buffer_capacity<T>(
    count: usize,
) -> Result<usize, AlignmentSummaryGpuPageAdmissionError> {
    if count == 0 {
        return Ok(0);
    }
    next_summary_capacity(count)?
        .checked_mul(size_of::<T>())
        .ok_or(AlignmentSummaryGpuPageAdmissionError::CapacityOverflow)
}

fn next_summary_capacity(count: usize) -> Result<usize, AlignmentSummaryGpuPageAdmissionError> {
    count
        .max(1)
        .checked_next_power_of_two()
        .ok_or(AlignmentSummaryGpuPageAdmissionError::CapacityOverflow)
}

/// Conservative retained charge for both the sorted summary GPU page table and the online B-tree
/// reclaim authority. The pointer-word allowance covers node links, occupancy slack, and
/// allocator metadata without depending on private standard-library layout.
fn summary_page_table_retained_bytes(capacity: usize) -> Option<usize> {
    let page_table = capacity.checked_mul(size_of::<GpuSummaryPage>())?;
    let reclaim_entry = size_of::<(u64, AlignmentSummaryGpuPageKey)>()
        .checked_add(4_usize.checked_mul(size_of::<usize>())?)?;
    page_table.checked_add(capacity.checked_mul(reclaim_entry)?)
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
    family: SummaryPageFamily,
    hidden_modification_mask: u32,
    modification_color_mode: ModifiedBaseColorMode,
) -> AlignmentSummaryGpuUniform {
    AlignmentSummaryGpuUniform {
        screen_size: [screen_size[0].max(1.0), screen_size[1].max(1.0)],
        clip_origin: [camera.clip().x as f32, camera.clip().y as f32],
        clip_size: [camera.clip().width as f32, camera.clip().height as f32],
        points_per_base: split_f64(1.0 / camera.bases_per_pixel()),
        page_start_from_camera: split_f64(page_start as f64 - camera.exact_start()),
        covered_clip_x,
        scale_maximum: scale_maximum.max(1) as f32,
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
            _ => u32::MAX,
        },
        hidden_modification_mask,
        modification_color_mode: match modification_color_mode {
            ModifiedBaseColorMode::ByType => 0,
            ModifiedBaseColorMode::Mono => 1,
            ModifiedBaseColorMode::ModifiedVsUnmodified => 2,
        },
        padding: 0,
    }
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
    if !matches!(
        alleles.interpretation,
        CoverageAlleleInterpretation::GenericReferenceAware
    ) {
        return Err(
            AlignmentSummaryGpuPrepareError::UnsupportedAlleleInterpretation(
                alleles.interpretation,
            ),
        );
    }
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
                hidden_modification_mask: 0,
                modification_color_mode: ModifiedBaseColorMode::ByType,
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
                hidden_modification_mask: metadata
                    .hidden_code_mask(&options.projection.hidden_codes),
                modification_color_mode: options.color_mode,
                background_range,
                foreground_range,
            })
        }
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
    };
    let selected_position = request.overlays.selected_position.or(option_selected);
    let selected = usize::from(
        selected_position.is_some_and(|position| request.camera.interval().contains(position)),
    );
    center.saturating_add(selected)
}

fn append_summary_overlays(
    request: &AlignmentSummaryGpuSceneRequest<'_>,
    overlays: &mut Vec<AlignmentSummaryGpuOverlayInstance>,
) {
    if summary_covered_interval(request).is_none() {
        return;
    }
    let clip = request.camera.clip();
    if matches!(
        &request.options,
        AlignmentSummaryGpuLaneOptions::Coverage(options) if options.depth.show_center_line
    ) {
        overlays.push(AlignmentSummaryGpuOverlayInstance {
            origin: [(clip.x + clip.width * 0.5) as f32, clip.y as f32],
            size: [1.0, clip.height as f32],
            color: [0.72, 0.75, 0.82, 0.72],
        });
    }
    let option_selected = match &request.options {
        AlignmentSummaryGpuLaneOptions::Coverage(options) => options.depth.selected_position,
        AlignmentSummaryGpuLaneOptions::Modifications(options) => options.selected_position,
    };
    if let Some(position) = request.overlays.selected_position.or(option_selected) {
        if request.camera.interval().contains(position) {
            let exact_start = request.camera.exact_start();
            overlays.push(AlignmentSummaryGpuOverlayInstance {
                origin: [
                    (clip.x + (position as f64 - exact_start) / request.camera.bases_per_pixel())
                        as f32,
                    clip.y as f32,
                ],
                size: [
                    (1.0 / request.camera.bases_per_pixel()).clamp(2.0, 12.0) as f32,
                    clip.height as f32,
                ],
                color: match request.options {
                    AlignmentSummaryGpuLaneOptions::Coverage(_) => [1.0, 0.78, 0.18, 0.92],
                    AlignmentSummaryGpuLaneOptions::Modifications(_) => [0.08, 0.08, 0.08, 0.34],
                },
            });
        }
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
    #[error("allele interpretation {0:?} is not supported by the generic substitution GPU slice")]
    UnsupportedAlleleInterpretation(CoverageAlleleInterpretation),
    #[error("summary page {0:?} contains a component from another family")]
    UnexpectedSemanticComponent(SummaryPageKey),
    #[error("summary page {page:?} has invalid semantic span {start}..{end}")]
    InvalidSemanticSpan {
        page: SummaryPageKey,
        start: u64,
        end: u64,
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
    #[error("allele interpretation {0:?} is not supported by the generic substitution GPU slice")]
    UnsupportedAlleleInterpretation(CoverageAlleleInterpretation),
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

fn shared_ledger_satisfies_goal(snapshot: &LedgerProgress, goal: GpuReclaimGoal) -> bool {
    goal.is_satisfied_at(snapshot.charged.saturating_add(snapshot.reserved))
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
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroU64},
        sync::Arc,
    };

    use rgv_alignment_scene::{
        AlignmentPageFamily, AlignmentPageKey, AlignmentPresentationScene, AlignmentReadLabelMode,
        AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSemanticPage, AlignmentSourceRevision,
        AlignmentStyleContract, AlleleSummaryDetail, AllocationBundleSlot, AllocationBundleSpec,
        AllocationClass, AllocationLedger, CanonicalGenomicTile, CanonicalRecordChunk,
        CanonicalRowBlock, CapabilityRequirement, CompleteCoverage, ContractFingerprint,
        CoverageAlleleInterpretation, CpuAlleleProjectionOptions, CpuDepthProjectionOptions,
        CpuModificationProjectionOptions, CpuSummaryProjectedComponent,
        CpuSummaryProjectionOptions, CpuSummaryProjector, EvidenceAcquisition, EvidenceChunk,
        EvidenceRevision, EvidenceRoot, EvidenceRootBuilder, ExactAlleleSummaryTile,
        ExactDepthSummaryTile, ExactFilterSummaryTile, ExactJunctionSummaryTile,
        ExactModificationSummaryTile, ExactPairSummaryTile, ExactSummaryRoot,
        FilteredRecordProvenance, GeometryLod, LayoutContract, PayloadContract, PayloadSidecar,
        PresentationContract, ReferenceRevision, ReservationKind, RevisionFingerprint,
        RowAtlasRevision, SceneRevision, SourceChunkId, SourceFingerprint, SummaryContract,
        SummaryRevision,
    };
    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, BisulfiteContext, ContigId,
        GenomicInterval, OverlappingMatePolicy, ReadStackingMode, Viewport,
    };
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, ReferenceSlice,
        SequenceData, TemplateLengthBounds,
    };

    use super::*;

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(77);

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn exact_authority_with_tile_bases(
        ledger: &AllocationLedger,
        tile_bases: NonZeroU64,
    ) -> (Arc<EvidenceRoot>, Arc<ExactSummaryRoot>) {
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 23,
            flags: AlignmentFlags::from_bits(0x63),
            mapping_quality: Some(60),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(50),
            template_length: 60,
            read_name: b"summary-gpu".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 3,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
            ],
            sequence: SequenceData::Present {
                bases: b"ACGTCACGTC".to_vec(),
                qualities: Some(vec![35; 10]),
            },
            auxiliary_fields: vec![
                AuxField {
                    tag: *b"MM",
                    value: AuxValue::String("C+m,0;C+h,0;C+a,0;".to_owned()),
                },
                AuxField {
                    tag: *b"ML",
                    value: AuxValue::UnsignedArray(vec![255, 128, 64]),
                },
                AuxField {
                    tag: *b"HP",
                    value: AuxValue::Unsigned(2),
                },
            ],
        };
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((5 << 16) | 1),
            )
            .unwrap();
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 5,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let payload_allocation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(payload_bytes, 1)
            .unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.core_bytes, 1)
            .unwrap();
        let overlap_allocation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            interval,
            FilteredRecordProvenance {
                records_seen: 1,
                eligible_records: 1,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(6, 7)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        let evidence = builder.build(2).unwrap();
        let contract = SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: AlleleSummaryDetail::Counts,
            collect_modifications: true,
        };
        let mut depth = Vec::new();
        let mut alleles = Vec::new();
        let mut junctions = Vec::new();
        let mut pairs = Vec::new();
        let mut filters = Vec::new();
        let mut modifications = Vec::new();
        for (index, tile) in CanonicalGenomicTile::covering(interval, tile_bases, 100)
            .unwrap()
            .into_iter()
            .enumerate()
        {
            let revision = 3_u64.saturating_add(index as u64);
            depth.push(
                ExactDepthSummaryTile::build(
                    &evidence,
                    tile,
                    100,
                    contract.depth(),
                    ledger,
                    ReservationKind::Normal,
                    revision,
                )
                .unwrap(),
            );
            alleles.push(
                ExactAlleleSummaryTile::build(
                    &evidence,
                    tile,
                    100,
                    contract.alleles(),
                    ledger,
                    ReservationKind::Normal,
                    revision,
                )
                .unwrap(),
            );
            junctions.push(
                ExactJunctionSummaryTile::build(
                    &evidence,
                    tile,
                    100,
                    contract.junctions(),
                    Some(1_000),
                    ledger,
                    ReservationKind::Normal,
                    revision,
                )
                .unwrap(),
            );
            pairs.push(
                ExactPairSummaryTile::build(
                    &evidence,
                    tile,
                    100,
                    contract.pairs(),
                    ledger,
                    ReservationKind::Normal,
                    revision,
                )
                .unwrap(),
            );
            filters.push(
                ExactFilterSummaryTile::build(
                    &evidence,
                    tile,
                    100,
                    interval,
                    contract.filters(),
                    ledger,
                    ReservationKind::Normal,
                    revision,
                )
                .unwrap(),
            );
            modifications.push(
                ExactModificationSummaryTile::build(
                    &evidence,
                    tile,
                    100,
                    contract.modifications().unwrap(),
                    ledger,
                    ReservationKind::Normal,
                    revision,
                )
                .unwrap(),
            );
        }
        let root = ExactSummaryRoot::build(
            SummaryRevision::new(RevisionFingerprint::new(8, 9)),
            evidence.revision(),
            evidence.source_revision(),
            evidence.record_contract(),
            contract,
            interval,
            depth,
            alleles,
            junctions,
            pairs,
            filters,
            modifications,
            ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();
        (evidence, root)
    }

    fn test_snapshot(ledger: &AllocationLedger) -> Arc<AlignmentSceneSnapshot> {
        test_snapshot_with_prepared_interval(ledger, None)
    }

    fn test_snapshot_with_prepared_interval(
        ledger: &AllocationLedger,
        prepared_interval: Option<GenomicInterval>,
    ) -> Arc<AlignmentSceneSnapshot> {
        test_snapshot_with_authority(ledger, prepared_interval, NonZeroU64::new(100).unwrap())
    }

    fn test_snapshot_with_summary_tile_bases(
        ledger: &AllocationLedger,
        tile_bases: NonZeroU64,
    ) -> Arc<AlignmentSceneSnapshot> {
        test_snapshot_with_authority(ledger, None, tile_bases)
    }

    fn test_snapshot_with_authority(
        ledger: &AllocationLedger,
        prepared_interval: Option<GenomicInterval>,
        tile_bases: NonZeroU64,
    ) -> Arc<AlignmentSceneSnapshot> {
        let (evidence, summaries) = exact_authority_with_tile_bases(ledger, tile_bases);
        let interval = summaries.prepared_interval();
        let prepared_interval = prepared_interval.unwrap_or(interval);
        let reference_revision = ReferenceRevision::new(RevisionFingerprint::new(30, 31));
        let reference = ReferenceSlice {
            interval,
            bases: vec![b'A'; interval.len() as usize],
        };
        let layout = LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(20, 21)),
            &evidence,
            interval,
            layout,
            ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: AlignmentStyleContract {
                read_labels: AlignmentReadLabelMode::Hidden,
                show_modified_bases: false,
                ..AlignmentStyleContract::default()
            },
            reference: Some(reference_revision),
            annotation: None,
        };
        let row_block = CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap());
        let mut read_pages = Vec::with_capacity(summaries.depth_tiles().len());
        for (index, tile) in summaries
            .depth_tiles()
            .iter()
            .map(|depth| depth.tile())
            .enumerate()
        {
            read_pages.push(
                AlignmentSemanticPage::build(
                    AlignmentPageKey::new(
                        evidence.source_revision(),
                        evidence.revision(),
                        atlas.revision(),
                        layout.fingerprint(),
                        presentation.page_fingerprint(AlignmentPageFamily::ReadGeometry),
                        tile,
                        row_block,
                        AlignmentPageFamily::ReadGeometry,
                        GeometryLod::Span,
                    ),
                    &evidence,
                    &atlas,
                    presentation,
                    100,
                    Some(&reference),
                    ledger,
                    ReservationKind::Normal,
                    21 + index as u64,
                )
                .unwrap(),
            );
        }
        let revision = SceneRevision::new(RevisionFingerprint::new(22, 23));
        let reads = AlignmentPresentationScene::new(
            revision,
            Arc::clone(&evidence),
            atlas,
            presentation,
            prepared_interval,
            0..1,
            read_pages,
            ledger,
            ReservationKind::Normal,
            22,
        )
        .unwrap();
        let mut pages = Vec::new();
        for tile in summaries.depth_tiles().iter().map(|depth| depth.tile()) {
            for family in [
                SummaryPageFamily::Depth,
                SummaryPageFamily::Alleles,
                SummaryPageFamily::Junctions,
                SummaryPageFamily::Modifications,
                SummaryPageFamily::Pairs,
                SummaryPageFamily::FilterProvenance,
            ] {
                let key = SummaryPageKey::new_with_dependencies(
                    &summaries,
                    tile,
                    family,
                    Some(reference_revision),
                    None,
                );
                let page = SummarySemanticPage::build_with_dependencies(
                    key,
                    &summaries,
                    (family == SummaryPageFamily::Alleles).then_some((
                        reference_revision,
                        &reference,
                        interval.end(),
                    )),
                    None,
                    ledger,
                    ReservationKind::Normal,
                    40 + pages.len() as u64,
                )
                .unwrap();
                pages.push(page);
            }
        }
        AlignmentSceneSnapshot::new(
            revision,
            reads,
            summaries,
            pages,
            ledger,
            ReservationKind::Normal,
            50,
        )
        .unwrap()
    }

    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("RGV alignment summary test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    fn camera(interval: GenomicInterval) -> CpuSummaryCamera {
        CpuSummaryCamera::new(
            interval,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 256.0,
                height: 96.0,
            },
        )
        .unwrap()
    }

    fn lane_options(alleles: bool) -> AlignmentCoverageLaneOptions {
        AlignmentCoverageLaneOptions {
            depth: CpuDepthProjectionOptions {
                scale_maximum: None,
                transform: CoverageScaleTransform::Linear,
                show_center_line: false,
                selected_position: None,
            },
            alleles: alleles.then_some(CpuAlleleProjectionOptions {
                minimum_fraction: 0.0,
                quality_weighted: false,
                scale_maximum: None,
                transform: CoverageScaleTransform::Linear,
                interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
            }),
        }
    }

    fn request<'a>(
        scene: &'a Arc<AlignmentSceneSnapshot>,
        sequence: u64,
        camera: CpuSummaryCamera,
        options: AlignmentCoverageLaneOptions,
    ) -> AlignmentSummaryGpuSceneRequest<'a> {
        AlignmentSummaryGpuSceneRequest {
            scene,
            camera_sequence: sequence,
            camera,
            screen_size_points: [256.0, 96.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 96,
            },
            options: options.into(),
            overlays: AlignmentSummaryFrameOverlays::default(),
        }
    }

    fn modification_options(
        probability_weighted: bool,
        hidden_codes: Vec<ModificationCode>,
        color_mode: ModifiedBaseColorMode,
    ) -> AlignmentModificationLaneOptions {
        AlignmentModificationLaneOptions {
            projection: CpuModificationProjectionOptions {
                probability_weighted,
                hidden_codes,
            },
            color_mode,
            selected_position: None,
        }
    }

    fn modification_request<'a>(
        scene: &'a Arc<AlignmentSceneSnapshot>,
        sequence: u64,
        camera: CpuSummaryCamera,
        options: AlignmentModificationLaneOptions,
    ) -> AlignmentSummaryGpuSceneRequest<'a> {
        AlignmentSummaryGpuSceneRequest {
            scene,
            camera_sequence: sequence,
            camera,
            screen_size_points: [256.0, 96.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 96,
            },
            options: options.into(),
            overlays: AlignmentSummaryFrameOverlays::default(),
        }
    }

    fn publish_bundled_summary_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut AlignmentSummaryGpuRenderer,
        requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        mut prior_frames: Vec<AlignmentSummaryGpuFrame>,
        first_slot: u32,
        publication_revision: u64,
    ) -> (
        AlignmentSummaryGpuFrame,
        AlignmentSummaryGpuPrepareStats,
        AlignmentSummaryGpuBatchAdmission,
    ) {
        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(requests, &snapshot, AllocationBundleSlot::new(first_slot))
            .unwrap();
        let retirement = renderer
            .detach_replacement_retirement(ledger, &plan, &mut prior_frames)
            .unwrap();
        assert!(prior_frames.is_empty());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        let bundle = ledger.reserve_replacement_bundle(spec).unwrap();
        let staged = renderer
            .stage_replacement(
                device,
                queue,
                requests,
                plan,
                &bundle,
                ledger,
                publication_revision,
            )
            .unwrap();
        let ready = renderer.preflight_replacement_publication(staged).unwrap();
        let actuals = ready.actuals().to_vec();
        let committed = bundle.commit(actuals).unwrap();
        let mut retirement = Some(retirement);
        let mut leases = committed.retire_and_settle(&mut retirement).unwrap();
        assert!(retirement.is_none());
        let publishable = renderer.bind_replacement_leases(ready, &mut leases);
        let published = renderer.publish_replacement(publishable);
        assert!(leases.is_empty());
        published
    }

    fn assert_near(left: f64, right: f64) {
        assert!((left - right).abs() < 1.0e-4, "{left} != {right}");
    }

    #[test]
    fn packed_depth_and_reference_aware_alleles_match_the_cpu_oracle() {
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let depth_page = scene
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Depth)
            .unwrap();
        let allele_page = scene
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Alleles)
            .unwrap();

        let depth_options = CpuDepthProjectionOptions {
            scale_maximum: Some(1),
            transform: CoverageScaleTransform::Linear,
            show_center_line: false,
            selected_position: None,
        };
        let cpu_depth = CpuSummaryProjector::project(
            &scene,
            1,
            camera,
            &CpuSummaryProjectionOptions::Depth(depth_options),
        )
        .unwrap();
        let PackedAlignmentSummaryPage::Depth {
            instances,
            range_maximum,
        } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::depth(
            depth_page,
        ))
        .unwrap()
        else {
            unreachable!();
        };
        let mut gpu_depth = instances
            .iter()
            .filter_map(|instance| {
                project_alignment_summary_depth_instance(
                    *instance,
                    depth_page.interval().start(),
                    camera,
                    1,
                    CoverageScaleTransform::Linear,
                )
            })
            .collect::<Vec<_>>();
        let mut cpu_depth = cpu_depth
            .primitives
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    CpuSummaryProjectedComponent::Depth { .. }
                )
            })
            .collect::<Vec<_>>();
        gpu_depth.sort_by(|left, right| left.x.total_cmp(&right.x));
        cpu_depth.sort_by(|left, right| left.x.total_cmp(&right.x));
        assert_eq!(gpu_depth.len(), cpu_depth.len());
        for (gpu, cpu) in gpu_depth.iter().zip(cpu_depth) {
            assert_near(gpu.x, cpu.x);
            assert_near(gpu.y, cpu.y);
            assert_near(gpu.width, cpu.width);
            assert_near(gpu.height, cpu.height);
        }
        assert_eq!(range_maximum.maximum_depth_exact(9.5, 10.0), 0);
        assert_eq!(range_maximum.maximum_depth_exact(9.5, 10.5), 1);
        assert_eq!(range_maximum.maximum_depth_exact(23.0, 23.5), 0);

        let allele_options = CpuAlleleProjectionOptions {
            minimum_fraction: 0.0,
            quality_weighted: false,
            scale_maximum: Some(1),
            transform: CoverageScaleTransform::Linear,
            interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
        };
        let cpu_alleles = CpuSummaryProjector::project(
            &scene,
            2,
            camera,
            &CpuSummaryProjectionOptions::Alleles(allele_options),
        )
        .unwrap();
        let PackedAlignmentSummaryPage::Alleles { instances } =
            PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::alleles(
                allele_page,
                depth_page,
            ))
            .unwrap()
        else {
            unreachable!();
        };
        let mut gpu_alleles = instances
            .iter()
            .filter_map(|instance| {
                project_alignment_summary_allele_instance(
                    *instance,
                    allele_page.interval().start(),
                    camera,
                    1,
                    CoverageScaleTransform::Linear,
                    0.0,
                    false,
                )
            })
            .collect::<Vec<_>>();
        let mut cpu_alleles = cpu_alleles
            .primitives
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    CpuSummaryProjectedComponent::Allele { .. }
                )
            })
            .collect::<Vec<_>>();
        gpu_alleles.sort_by(|left, right| {
            left.bounds
                .x
                .total_cmp(&right.bounds.x)
                .then_with(|| left.bounds.y.total_cmp(&right.bounds.y))
        });
        cpu_alleles.sort_by(|left, right| {
            left.x
                .total_cmp(&right.x)
                .then_with(|| left.y.total_cmp(&right.y))
        });
        assert_eq!(gpu_alleles.len(), cpu_alleles.len());
        for (gpu, cpu) in gpu_alleles.iter().zip(cpu_alleles) {
            assert_near(gpu.bounds.x, cpu.x);
            assert_near(gpu.bounds.y, cpu.y);
            assert_near(gpu.bounds.width, cpu.width);
            assert_near(gpu.bounds.height, cpu.height);
        }
    }

    #[test]
    fn packed_modifications_match_the_dominant_cpu_oracle_for_live_lane_options() {
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let page = scene
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Modifications)
            .unwrap();
        assert!(page.primitives().iter().any(|primitive| {
            matches!(
                primitive.component,
                SummarySemanticComponent::Modification { bin, .. } if bin.modified_calls == 0
            )
        }));
        let PackedAlignmentSummaryPage::Modifications {
            instances,
            metadata,
        } = PackedAlignmentSummaryPage::from_request(
            AlignmentSummaryGpuPageRequest::modifications(page),
        )
        .unwrap()
        else {
            unreachable!();
        };
        assert_eq!(
            metadata.codes(),
            &[
                ModificationCode::Letter(b'a'),
                ModificationCode::Letter(b'h'),
                ModificationCode::Letter(b'm'),
            ]
        );
        assert!(
            instances
                .iter()
                .filter(|instance| instance.shape == 0)
                .all(|instance| {
                    instance.modified_calls > 0 && instance.canonical_observations > 0
                })
        );
        assert_eq!(metadata.background_range().len(), 1);
        assert_eq!(metadata.raw_range().len(), 3);
        assert_eq!(metadata.weighted_range().len(), 3);

        let cases = [
            (false, Vec::new(), Some(ModificationCode::Letter(b'a'))),
            (
                false,
                vec![ModificationCode::Letter(b'a')],
                Some(ModificationCode::Letter(b'h')),
            ),
            (true, Vec::new(), Some(ModificationCode::Letter(b'm'))),
            (
                true,
                vec![ModificationCode::Letter(b'm')],
                Some(ModificationCode::Letter(b'h')),
            ),
            (
                true,
                vec![
                    ModificationCode::Letter(b'm'),
                    ModificationCode::Letter(b'h'),
                ],
                Some(ModificationCode::Letter(b'a')),
            ),
            (
                true,
                vec![
                    ModificationCode::Letter(b'm'),
                    ModificationCode::Letter(b'h'),
                    ModificationCode::Letter(b'a'),
                ],
                None,
            ),
        ];
        for (probability_weighted, hidden_codes, expected_code) in cases {
            let options = CpuModificationProjectionOptions {
                probability_weighted,
                hidden_codes: hidden_codes.clone(),
            };
            let cpu =
                CpuSummaryProjector::project_dominant_modifications(&scene, 1, camera, &options)
                    .unwrap();
            let range = if probability_weighted {
                metadata.weighted_range()
            } else {
                metadata.raw_range()
            };
            let hidden_mask = metadata.hidden_code_mask(&hidden_codes);
            let mut gpu = instances[range.start as usize..range.end as usize]
                .iter()
                .filter_map(|instance| {
                    project_alignment_summary_modification_instance(
                        *instance,
                        page.interval().start(),
                        camera,
                        probability_weighted,
                        hidden_mask,
                    )
                })
                .collect::<Vec<_>>();
            let mut cpu = cpu
                .primitives
                .iter()
                .filter_map(|primitive| {
                    let CpuSummaryProjectedComponent::Modification {
                        series,
                        bin,
                        fraction_bits,
                    } = primitive.component
                    else {
                        return None;
                    };
                    Some((primitive, series, bin, f64::from_bits(fraction_bits)))
                })
                .collect::<Vec<_>>();
            gpu.sort_by(|left, right| left.bounds.x.total_cmp(&right.bounds.x));
            cpu.sort_by(|left, right| left.0.x.total_cmp(&right.0.x));
            assert_eq!(gpu.len(), cpu.len());
            assert_eq!(
                gpu.first()
                    .map(|projected| metadata.codes()[projected.code_index as usize]),
                expected_code
            );
            for (gpu, (cpu, series, bin, fraction)) in gpu.iter().zip(cpu) {
                assert_eq!(metadata.codes()[gpu.code_index as usize], series.code);
                assert_eq!(gpu.modified_calls, bin.modified_calls);
                assert_eq!(gpu.canonical_observations, bin.canonical_observations);
                assert_near(gpu.fraction, fraction);
                assert_near(gpu.bounds.x, cpu.x);
                assert_near(gpu.bounds.y, cpu.y);
                assert_near(gpu.bounds.width, cpu.width);
                assert_near(gpu.bounds.height, cpu.height);
            }
        }

        let raw_style = summary_draw_style(
            &AlignmentSummaryGpuLaneOptions::Modifications(modification_options(
                false,
                vec![ModificationCode::Letter(b'a')],
                ModifiedBaseColorMode::ByType,
            )),
            AlignmentSummaryGpuPageKey::modifications(page).unwrap(),
            &GpuSummaryPageMetadata::Modifications(metadata.clone()),
            instances.len() as u32,
        )
        .unwrap();
        assert_eq!(raw_style.background_range, 0..0);
        assert_eq!(raw_style.foreground_range, metadata.raw_range());
        assert_eq!(
            raw_style.hidden_modification_mask,
            metadata.hidden_code_mask(&[ModificationCode::Letter(b'a')])
        );
        let weighted_background_style = summary_draw_style(
            &AlignmentSummaryGpuLaneOptions::Modifications(modification_options(
                true,
                Vec::new(),
                ModifiedBaseColorMode::ModifiedVsUnmodified,
            )),
            AlignmentSummaryGpuPageKey::modifications(page).unwrap(),
            &GpuSummaryPageMetadata::Modifications(metadata.clone()),
            instances.len() as u32,
        )
        .unwrap();
        assert_eq!(
            weighted_background_style.background_range,
            metadata.background_range()
        );
        assert_eq!(
            weighted_background_style.foreground_range,
            metadata.weighted_range()
        );
    }

    #[test]
    fn canonical_gpu_pages_are_clipped_to_the_snapshot_prepared_interval() {
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let prepared = GenomicInterval::new(ContigId::new(0), 12, 20).unwrap();
        let scene = test_snapshot_with_prepared_interval(&ledger, Some(prepared));
        let camera = camera(GenomicInterval::new(ContigId::new(0), 0, 100).unwrap());
        let request = request(&scene, 1, camera, lane_options(false));
        let covered = summary_covered_interval(&request).unwrap();
        assert_eq!(covered, prepared);
        let clip = summary_covered_clip_x(camera, covered);
        assert_near(f64::from(clip[0]), 30.72);
        assert_near(f64::from(clip[1]), 51.2);

        let cpu = CpuSummaryProjector::project(
            &scene,
            1,
            camera,
            &CpuSummaryProjectionOptions::Depth(CpuDepthProjectionOptions {
                scale_maximum: Some(1),
                transform: CoverageScaleTransform::Linear,
                show_center_line: false,
                selected_position: None,
            }),
        )
        .unwrap();
        let depth_page = scene
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Depth)
            .unwrap();
        let PackedAlignmentSummaryPage::Depth { instances, .. } =
            PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::depth(
                depth_page,
            ))
            .unwrap()
        else {
            unreachable!();
        };
        let mut gpu = instances
            .iter()
            .filter_map(|instance| {
                projected_horizontal_in_clip(
                    instance.start_offset,
                    instance.end_offset,
                    depth_page.interval().start(),
                    camera,
                    [f64::from(clip[0]), f64::from(clip[1])],
                )
            })
            .collect::<Vec<_>>();
        let mut cpu = cpu
            .primitives
            .iter()
            .filter_map(|primitive| {
                matches!(
                    primitive.component,
                    CpuSummaryProjectedComponent::Depth { .. }
                )
                .then_some((primitive.x, primitive.width))
            })
            .collect::<Vec<_>>();
        gpu.sort_by(|left, right| left.0.total_cmp(&right.0));
        cpu.sort_by(|left, right| left.0.total_cmp(&right.0));
        assert_eq!(gpu.len(), cpu.len());
        for ((gpu_x, gpu_width), (cpu_x, cpu_width)) in gpu.into_iter().zip(cpu) {
            assert_near(gpu_x, cpu_x);
            assert_near(gpu_width, cpu_width);
            assert!(gpu_x >= f64::from(clip[0]));
            assert!(gpu_x + gpu_width <= f64::from(clip[1]) + 1.0);
        }

        let visible = summary_requests_for_scene(&request, true).unwrap();
        assert_eq!(visible.len(), 1);
        let uniform = summary_uniform(
            camera,
            [256.0, 96.0],
            depth_page.interval().start(),
            clip,
            1,
            0.0,
            CoverageScaleTransform::Linear,
            false,
            SummaryPageFamily::Depth,
            0,
            ModifiedBaseColorMode::ByType,
        );
        assert_eq!(uniform.covered_clip_x, clip);
    }

    #[test]
    fn overlays_require_scene_overlap_and_selected_marker_matches_cpu_geometry() {
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let viewport = Viewport::new(scene.prepared_interval(), 256.0)
            .unwrap()
            .pan_pixels(1.28, 1_000)
            .unwrap();
        let live_camera = CpuSummaryCamera::from_viewport(
            viewport,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 256.0,
                height: 96.0,
            },
        )
        .unwrap();
        let mut options = lane_options(false);
        // The CPU oracle uses the enclosing integer camera interval as marker authority. A
        // fractional pan can therefore retain the partially scissored leading-base marker.
        options.depth.selected_position = Some(live_camera.interval().start());
        let cpu = CpuSummaryProjector::project(
            &scene,
            1,
            live_camera,
            &CpuSummaryProjectionOptions::Depth(options.depth),
        )
        .unwrap();
        let cpu_marker = cpu
            .primitives
            .iter()
            .find(|primitive| {
                matches!(
                    primitive.component,
                    CpuSummaryProjectedComponent::CoverageSelectedPosition { .. }
                )
            })
            .unwrap();
        let live_request = request(&scene, 1, live_camera, options);
        let mut overlays = Vec::new();
        append_summary_overlays(&live_request, &mut overlays);
        assert_eq!(overlays.len(), 1);
        assert_near(f64::from(overlays[0].origin[0]), cpu_marker.x);
        assert_near(f64::from(overlays[0].origin[1]), cpu_marker.y);
        assert_near(f64::from(overlays[0].size[0]), cpu_marker.width);
        assert_near(f64::from(overlays[0].size[1]), cpu_marker.height);

        let disjoint_camera = camera(GenomicInterval::new(ContigId::new(0), 100, 200).unwrap());
        let disjoint = request(&scene, 2, disjoint_camera, options);
        assert_eq!(summary_overlay_count(&disjoint), 0);
        let mut disjoint_overlays = Vec::new();
        append_summary_overlays(&disjoint, &mut disjoint_overlays);
        assert!(disjoint_overlays.is_empty());
        assert!(
            summary_requests_for_scene(&disjoint, true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn batch_admission_is_atomic_and_all_device_pages_are_ledger_accounted() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let owned = unique_summary_requests(
            &[request(
                &scene,
                1,
                camera(scene.prepared_interval()),
                lane_options(true),
            )],
            true,
        )
        .unwrap();
        let borrowed = borrowed_summary_requests(&owned);
        assert_eq!(borrowed.len(), 2);
        let baseline = ledger.snapshot().unwrap().charged;
        ledger.set_budget(baseline.saturating_add(1)).unwrap();
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert!(matches!(
            renderer.admit_pages(&device, &queue, &borrowed, &ledger, 60),
            Err(AlignmentSummaryGpuPageAdmissionError::Ledger(
                LedgerError::BudgetExceeded { .. }
            ))
        ));
        assert_eq!(renderer.resident_page_count(), 0);

        ledger.set_budget(64 << 20).unwrap();
        let admission = renderer
            .admit_pages(&device, &queue, &borrowed, &ledger, 61)
            .unwrap();
        assert_eq!(admission.admitted_pages, 2);
        assert!(admission.uploaded_bytes > 0);
        assert_eq!(renderer.resident_page_count(), 2);
        assert_eq!(renderer.reclaim_index.len(), renderer.pages.len());
        assert_eq!(
            renderer
                .page_table_allocation
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap()
                .charged_capacity,
            summary_page_table_retained_bytes(renderer.page_table_capacity).unwrap()
        );
        let after = ledger.snapshot().unwrap();
        assert!(after.charged > baseline);
        assert!(after.records.iter().any(|record| {
            record.class == AllocationClass::GpuPage && record.charged_capacity > 0
        }));
    }

    #[test]
    fn fractional_camera_frames_reuse_pages_and_render_with_uniform_only_updates() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let navigation_interval = GenomicInterval::new(ContigId::new(0), 0, 50).unwrap();
        let first_camera = camera(navigation_interval);
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (first, first_stats, first_admission) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 1, first_camera, lane_options(true))],
                &ledger,
                70,
            )
            .unwrap();
        assert_eq!(first_admission.admitted_pages, 2);
        assert!(first_stats.geometry_uploaded_bytes > 0);
        assert_eq!(first_stats.range_maximum_queries, 1);
        assert_eq!(first.draw_count(), 2);
        render_test_frame(&device, &queue, &renderer, &first).unwrap();
        drop(first);

        let viewport = Viewport::new(navigation_interval, 256.0)
            .unwrap()
            .pan_pixels(0.5, 100)
            .unwrap()
            .zoom_at(128.0, 0.9, 100)
            .unwrap();
        assert!(viewport.exact_start() > 0.0);
        let panned = CpuSummaryCamera::from_viewport(
            viewport,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 256.0,
                height: 96.0,
            },
        )
        .unwrap();
        let mut changed_options = lane_options(true);
        changed_options.depth.transform = CoverageScaleTransform::SquareRoot;
        changed_options.depth.show_center_line = true;
        changed_options.depth.selected_position = Some(12);
        let mut changed_alleles = changed_options.alleles.unwrap();
        changed_alleles.transform = CoverageScaleTransform::SquareRoot;
        changed_alleles.quality_weighted = true;
        changed_alleles.minimum_fraction = 0.25;
        changed_options.alleles = Some(changed_alleles);
        let (second, second_stats, second_admission) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 2, panned, changed_options)],
                &ledger,
                71,
            )
            .unwrap();
        assert_eq!(second_admission.admitted_pages, 0);
        assert_eq!(second_admission.already_resident_pages, 2);
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
        assert_eq!(second_stats.overlay_instances, 2);
        assert_eq!(second.overlay_draw_count(), 1);
        assert_eq!(renderer.resident_page_count(), 2);
        render_test_frame(&device, &queue, &renderer, &second).unwrap();
        renderer.discard_device_resources();
        assert_eq!(renderer.resident_page_count(), 0);
        assert!(matches!(
            render_test_frame(&device, &queue, &renderer, &second),
            Err(AlignmentSummaryGpuRenderError::DeviceGenerationMismatch { .. })
        ));
    }

    #[test]
    fn camera_scale_and_overlay_captures_preserve_summary_authority_without_a_bundle() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
        let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let first_camera = camera(GenomicInterval::new(ContigId::new(0), 0, 20).unwrap());
        let first_requests = [request(&scene, 1, first_camera, lane_options(true))];
        let (first, _, _) = publish_bundled_summary_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            Vec::new(),
            500,
            600,
        );
        renderer
            .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 601)
            .unwrap();
        let authority = first.content_authority().clone();
        drop(first);
        let page_allocations = renderer
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
            .collect::<Vec<_>>();
        let page_table_allocation = renderer
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id);

        let mut square_root = lane_options(true);
        square_root.depth.transform = CoverageScaleTransform::SquareRoot;
        square_root.depth.show_center_line = true;
        let mut square_root_alleles = square_root.alleles.unwrap();
        square_root_alleles.transform = CoverageScaleTransform::SquareRoot;
        square_root_alleles.minimum_fraction = 0.2;
        square_root.alleles = Some(square_root_alleles);
        let mut selected = lane_options(true);
        selected.depth.selected_position = Some(42);
        let camera_options = [
            (
                camera(GenomicInterval::new(ContigId::new(0), 5, 25).unwrap()),
                lane_options(true),
            ),
            (
                camera(GenomicInterval::new(ContigId::new(0), 15, 35).unwrap()),
                square_root,
            ),
            (
                camera(GenomicInterval::new(ContigId::new(0), 35, 55).unwrap()),
                selected,
            ),
        ];
        let operations_before = ledger.operation_counters().unwrap();
        let renderer_operations_before = renderer.operation_counters();
        for (index, (camera, options)) in camera_options.into_iter().enumerate() {
            assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
            let requests = [request(&scene, 2 + index as u64, camera, options)];
            let (frame, stats) = renderer
                .capture_camera_frame(&device, &queue, &authority, &requests, &ledger)
                .unwrap();
            assert_eq!(stats.geometry_uploaded_bytes, 0);
            assert_eq!(stats.page_cache_misses, 0);
            assert_eq!(frame.content_authority(), &authority);
            assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
            assert_eq!(
                renderer
                    .pages
                    .iter()
                    .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                    .collect::<Vec<_>>(),
                page_allocations
            );
            assert_eq!(
                renderer
                    .page_table_allocation
                    .as_ref()
                    .map(AllocationLease::id),
                page_table_allocation
            );
            drop(frame);
        }
        let operations_after = ledger.operation_counters().unwrap();
        let renderer_operations_after = renderer.operation_counters();
        assert_eq!(
            operations_after.full_inventory_snapshots,
            operations_before.full_inventory_snapshots
        );
        assert_eq!(
            operations_after.allocation_records_enumerated,
            operations_before.allocation_records_enumerated
        );
        assert_eq!(
            renderer_operations_after, renderer_operations_before,
            "camera, scale, and overlay capture must not rebuild the full prepared summary manifest"
        );
        assert!(renderer.frame_buffers.len() <= 2);

        let changed_scene_set = [
            request(&scene, 7, first_camera, lane_options(true)),
            request(&scene, 8, first_camera, lane_options(true)),
        ];
        assert!(matches!(
            renderer
                .capture_camera_frame(&device, &queue, &authority, &changed_scene_set, &ledger,),
            Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::SceneContent { .. }
                }
            )
        ));

        let modification_requests = [modification_request(
            &scene,
            9,
            first_camera,
            modification_options(false, Vec::new(), ModifiedBaseColorMode::ByType),
        )];
        assert!(matches!(
            renderer.capture_camera_frame(
                &device,
                &queue,
                &authority,
                &modification_requests,
                &ledger,
            ),
            Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::ContentContract { .. }
                }
            )
        ));
        let (replacement, replacement_stats, _) = publish_bundled_summary_frame(
            &device,
            &queue,
            &mut renderer,
            &modification_requests,
            &ledger,
            Vec::new(),
            540,
            640,
        );
        assert!(replacement_stats.geometry_uploaded_bytes > 0);
        assert!(replacement.draw_count() > 0);

        let replacement_authority = replacement.content_authority().clone();
        drop(replacement);
        renderer.discard_device_resources();
        assert!(matches!(
            renderer.capture_camera_frame(
                &device,
                &queue,
                &replacement_authority,
                &modification_requests,
                &ledger,
            ),
            Err(
                AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentSummaryGpuContentReplacementReason::DeviceGeneration { .. }
                }
            )
        ));
    }

    #[test]
    fn compatible_summary_capture_uses_the_precharged_ring_during_an_unrelated_bundle() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
        let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let first_camera = camera(GenomicInterval::new(ContigId::new(0), 0, 20).unwrap());
        let first_requests = [request(&scene, 1, first_camera, lane_options(true))];
        let (first, _, _) = publish_bundled_summary_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            Vec::new(),
            900,
            901,
        );
        renderer
            .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 902)
            .unwrap();
        let authority = first.content_authority().clone();
        assert_eq!(
            renderer.frame_buffers.len(),
            ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT
        );
        let slot_ids = renderer
            .frame_buffers
            .iter()
            .map(|slot| {
                (
                    slot.uniform_allocation.as_ref().map(AllocationLease::id),
                    slot.overlay_allocation.as_ref().map(AllocationLease::id),
                    slot.frame_allocation.id(),
                )
            })
            .collect::<Vec<_>>();

        let mut options = lane_options(true);
        options.depth.show_center_line = true;
        options.depth.selected_position = Some(42);
        let requests = [request(
            &scene,
            2,
            camera(scene.prepared_interval()),
            options,
        )];
        let policy_revision = ledger.snapshot().unwrap().policy_revision;
        let unrelated = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                policy_revision,
                Vec::new(),
                ledger.empty_retirement_claim_set(),
            ))
            .unwrap();
        let before = ledger.snapshot().unwrap();
        let allocation_ids_before = before
            .records
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        let (second, stats) = renderer
            .capture_camera_frame(&device, &queue, &authority, &requests, &ledger)
            .unwrap();
        assert!(stats.mandatory_pages > first.draw_count());
        assert_eq!(stats.overlay_instances, 2);
        assert_eq!(stats.geometry_uploaded_bytes, 0);
        assert_ne!(
            first.buffers.frame_allocation.id(),
            second.buffers.frame_allocation.id()
        );
        assert!(matches!(
            renderer.capture_camera_frame(&device, &queue, &authority, &requests, &ledger),
            Err(AlignmentSummaryGpuCameraCaptureError::FrameSlotBusy { slots: 2 })
        ));
        let after = ledger.snapshot().unwrap();
        assert_eq!(
            after.active_replacement_bundle,
            before.active_replacement_bundle
        );
        assert_eq!(after.charged, before.charged);
        assert_eq!(after.reserved, before.reserved);
        assert_eq!(
            after
                .records
                .iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            allocation_ids_before
        );
        assert_eq!(
            renderer
                .frame_buffers
                .iter()
                .map(|slot| (
                    slot.uniform_allocation.as_ref().map(AllocationLease::id),
                    slot.overlay_allocation.as_ref().map(AllocationLease::id),
                    slot.frame_allocation.id(),
                ))
                .collect::<Vec<_>>(),
            slot_ids
        );
        drop(unrelated);
        drop(second);
        drop(first);
    }

    #[test]
    fn bundled_replacement_combines_multiple_lanes_and_reuses_unchanged_geometry() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (old_depth, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 1, camera, lane_options(false))],
                &ledger,
                200,
            )
            .unwrap();
        let (old_alleles, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 2, camera, lane_options(true))],
                &ledger,
                201,
            )
            .unwrap();
        render_test_frame(&device, &queue, &renderer, &old_depth).unwrap();
        render_test_frame(&device, &queue, &renderer, &old_alleles).unwrap();

        let candidate_requests = [
            request(&scene, 3, camera, lane_options(false)),
            request(&scene, 4, camera, lane_options(true)),
        ];
        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(
                &candidate_requests,
                &snapshot,
                AllocationBundleSlot::new(40),
            )
            .unwrap();
        assert_eq!(plan.first_bundle_slot(), AllocationBundleSlot::new(40));
        let mut old_frames = vec![old_depth, old_alleles];
        let retirement = renderer
            .detach_replacement_retirement(&ledger, &plan, &mut old_frames)
            .unwrap();
        assert!(old_frames.is_empty());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        let bundle = ledger.reserve_replacement_bundle(spec).unwrap();
        let reserved = ledger.snapshot().unwrap();
        assert_eq!(reserved.reserved_transition, bundle.maximum_bytes());
        let staged = renderer
            .stage_replacement(
                &device,
                &queue,
                &candidate_requests,
                plan,
                &bundle,
                &ledger,
                202,
            )
            .unwrap();
        assert_eq!(staged.geometry_uploaded_bytes, 0);
        let ready = renderer.preflight_replacement_publication(staged).unwrap();
        let committed = bundle.commit(ready.actuals().to_vec()).unwrap();
        let mut retirement = Some(retirement);
        let mut leases = committed.retire_and_settle(&mut retirement).unwrap();
        let publishable = renderer.bind_replacement_leases(ready, &mut leases);
        let (candidate, stats, admission) = renderer.publish_replacement(publishable);
        assert!(leases.is_empty());
        assert_eq!(candidate.scene_revisions.len(), 2);
        assert_eq!(candidate.camera_sequences.as_ref(), [3, 4]);
        assert_eq!(candidate.draw_count(), 3);
        assert_eq!(admission.admitted_pages, 0);
        assert_eq!(stats.geometry_uploaded_bytes, 0);
        render_test_frame(&device, &queue, &renderer, &candidate).unwrap();
    }

    #[test]
    fn bundled_summary_replacements_preserve_multi_quantum_prefetch_without_repeat_uploads() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
        let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let first_camera = camera(GenomicInterval::new(ContigId::new(0), 0, 20).unwrap());
        let first_requests = [request(&scene, 1, first_camera, lane_options(true))];
        let prepared_keys =
            AlignmentSummaryGpuRenderer::prepared_page_keys(&first_requests).unwrap();
        assert!(prepared_keys.len() >= 10);
        let (first, _, first_admission) = publish_bundled_summary_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            Vec::new(),
            300,
            400,
        );
        assert!(first_admission.admitted_pages < prepared_keys.len());

        let mut admitted_quanta = 0_usize;
        let mut previous_resident = renderer.resident_page_count();
        loop {
            let progress = renderer
                .prefetch_prepared_scene_pages_bounded(
                    &device,
                    &queue,
                    &first_requests,
                    &ledger,
                    401 + admitted_quanta as u64,
                    NonZeroUsize::new(1).unwrap(),
                )
                .unwrap();
            assert!(progress.admission.admitted_pages <= 1);
            let resident = renderer.resident_page_count();
            assert!(resident >= previous_resident);
            if progress.admission.admitted_pages > 0 {
                admitted_quanta = admitted_quanta.saturating_add(1);
                assert_eq!(resident, previous_resident.saturating_add(1));
            }
            previous_resident = resident;
            if progress.is_complete() {
                break;
            }
        }
        assert!(admitted_quanta > 2);
        assert_eq!(renderer.resident_page_count(), prepared_keys.len());
        let converged_page_ids = renderer
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
            .collect::<Vec<_>>();

        let repeated_requests = [request(&scene, 2, first_camera, lane_options(true))];
        let (second, second_stats, second_admission) = publish_bundled_summary_frame(
            &device,
            &queue,
            &mut renderer,
            &repeated_requests,
            &ledger,
            vec![first],
            360,
            440,
        );
        assert_eq!(second_admission.admitted_pages, 0);
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
        assert_eq!(renderer.resident_page_count(), prepared_keys.len());
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                .collect::<Vec<_>>(),
            converged_page_ids
        );
        let no_repeat = renderer
            .prefetch_prepared_scene_pages_bounded(
                &device,
                &queue,
                &repeated_requests,
                &ledger,
                441,
                NonZeroUsize::new(1).unwrap(),
            )
            .unwrap();
        assert!(no_repeat.is_complete());
        assert_eq!(no_repeat.admission.admitted_pages, 0);
        assert_eq!(no_repeat.admission.uploaded_bytes, 0);

        let compatible_camera = camera(GenomicInterval::new(ContigId::new(0), 20, 40).unwrap());
        let compatible_requests = [request(&scene, 3, compatible_camera, lane_options(true))];
        let (third, third_stats, third_admission) = publish_bundled_summary_frame(
            &device,
            &queue,
            &mut renderer,
            &compatible_requests,
            &ledger,
            vec![second],
            420,
            442,
        );
        assert_eq!(third_admission.admitted_pages, 0);
        assert_eq!(third_stats.geometry_uploaded_bytes, 0);
        assert_eq!(renderer.resident_page_count(), prepared_keys.len());
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                .collect::<Vec<_>>(),
            converged_page_ids
        );
        render_test_frame(&device, &queue, &renderer, &third).unwrap();
    }

    #[test]
    fn summary_prefetch_cursor_resolves_once_then_advances_by_quantum() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
        let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let requests = [request(
            &scene,
            1,
            camera(scene.prepared_interval()),
            lane_options(true),
        )];
        let keys: Arc<[AlignmentSummaryGpuPageKey]> =
            AlignmentSummaryGpuRenderer::prepared_page_keys(&requests)
                .unwrap()
                .into();
        let before = renderer.operation_counters();
        let mut cursor = renderer
            .begin_prepared_scene_prefetch_cursor(&requests, Arc::clone(&keys))
            .unwrap();
        let after_begin = renderer.operation_counters();
        assert_eq!(after_begin, before);

        let mut revision = 50_u64;
        while !cursor.is_complete() {
            let before_step = renderer.operation_counters();
            let progress = renderer
                .advance_prepared_scene_prefetch_cursor(
                    &device,
                    &queue,
                    &mut cursor,
                    &ledger,
                    revision,
                    NonZeroUsize::new(2).unwrap(),
                )
                .unwrap();
            assert!(progress.admission.requested_pages <= 2);
            let after_step = renderer.operation_counters();
            assert_eq!(
                after_step.content_manifest_builds,
                before_step.content_manifest_builds
            );
            assert_eq!(
                after_step.content_manifest_pages_enumerated,
                before_step.content_manifest_pages_enumerated
            );
            assert!(
                after_step
                    .prefetch_authority_keys_inspected
                    .saturating_sub(before_step.prefetch_authority_keys_inspected)
                    <= 2
            );
            revision = revision.saturating_add(1);
        }
        let residency = renderer
            .finish_prepared_scene_prefetch_cursor(cursor)
            .unwrap();
        assert_eq!(residency.page_keys.as_ref(), keys.as_ref());
        assert_eq!(renderer.resident_page_count(), keys.len());
    }

    #[test]
    fn refused_summary_bundle_restores_exact_frames_and_cache_ownership() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let old_scene = test_snapshot(&ledger);
        let candidate_scene =
            test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
        let current_camera = camera(old_scene.prepared_interval());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (prior, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&old_scene, 1, current_camera, lane_options(true))],
                &ledger,
                210,
            )
            .unwrap();
        let page_keys = renderer
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>();
        let page_ids = renderer
            .pages
            .iter()
            .map(|page| page.allocation.as_ref().map(AllocationLease::id))
            .collect::<Vec<_>>();
        let table_id = renderer
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id);
        let table_capacity = renderer.page_table_capacity;
        let frame_buffer_ptrs = renderer
            .frame_buffers
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>();

        let occupied = ledger.snapshot().unwrap().charged;
        ledger.set_limits(occupied, 0, 16 << 20).unwrap();
        let candidate_camera = camera(GenomicInterval::new(ContigId::new(0), 25, 50).unwrap());
        let candidate_requests = [request(
            &candidate_scene,
            2,
            candidate_camera,
            lane_options(true),
        )];
        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(
                &candidate_requests,
                &snapshot,
                AllocationBundleSlot::new(80),
            )
            .unwrap();
        let mut prior_frames = vec![prior];
        let foreign_ledger = AllocationLedger::new(1 << 20, 1 << 20);
        assert!(matches!(
            renderer.detach_replacement_retirement(&foreign_ledger, &plan, &mut prior_frames),
            Err(AlignmentSummaryGpuReplacementError::Bundle(
                AllocationBundleError::ForeignRetirementReference { .. }
            ))
        ));
        assert_eq!(prior_frames.len(), 1);
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.key)
                .collect::<Vec<_>>(),
            page_keys
        );
        assert_eq!(
            renderer
                .frame_buffers
                .iter()
                .map(Arc::as_ptr)
                .collect::<Vec<_>>(),
            frame_buffer_ptrs
        );
        render_test_frame(&device, &queue, &renderer, &prior_frames[0]).unwrap();
        let retirement = renderer
            .detach_replacement_retirement(&ledger, &plan, &mut prior_frames)
            .unwrap();
        assert!(prior_frames.is_empty());
        assert_eq!(retirement.retired_pages.len(), page_keys.len());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        assert!(matches!(
            ledger.reserve_replacement_bundle(spec),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. }
                | AllocationBundleError::TransitionOverlapBudgetExceeded { .. })
        ));
        let restored = renderer.restore_replacement_retirement(retirement).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.key)
                .collect::<Vec<_>>(),
            page_keys
        );
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.allocation.as_ref().map(AllocationLease::id))
                .collect::<Vec<_>>(),
            page_ids
        );
        assert_eq!(
            renderer
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            table_id
        );
        assert_eq!(renderer.page_table_capacity, table_capacity);
        assert_eq!(
            renderer
                .frame_buffers
                .iter()
                .map(Arc::as_ptr)
                .collect::<Vec<_>>(),
            frame_buffer_ptrs
        );
        render_test_frame(&device, &queue, &renderer, &restored[0]).unwrap();
    }

    #[test]
    fn summary_bundle_rejects_first_frame_and_retired_frame_that_cannot_fit_steady_budget() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 64 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let requests = [request(&scene, 1, camera, lane_options(true))];
        let probe = renderer
            .plan_replacement(
                &requests,
                &ledger.snapshot().unwrap(),
                AllocationBundleSlot::new(120),
            )
            .unwrap();
        let maximum = probe
            .demands()
            .iter()
            .map(|demand| demand.maximum_bytes)
            .sum::<usize>();
        let baseline = ledger.snapshot().unwrap().charged;
        ledger
            .set_limits(
                baseline.saturating_add(maximum).saturating_sub(1),
                64 << 20,
                16 << 20,
            )
            .unwrap();
        let first_plan = renderer
            .plan_replacement(
                &requests,
                &ledger.snapshot().unwrap(),
                AllocationBundleSlot::new(120),
            )
            .unwrap();
        let first_spec = AllocationBundleSpec::new(
            first_plan.policy_revision(),
            first_plan.demands().to_vec(),
            ledger.empty_retirement_claim_set(),
        );
        assert!(matches!(
            ledger.reserve_replacement_bundle(first_spec),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. })
        ));

        ledger.set_limits(64 << 20, 64 << 20, 16 << 20).unwrap();
        let first_plan = renderer
            .plan_replacement(
                &requests,
                &ledger.snapshot().unwrap(),
                AllocationBundleSlot::new(140),
            )
            .unwrap();
        let first_spec = AllocationBundleSpec::new(
            first_plan.policy_revision(),
            first_plan.demands().to_vec(),
            ledger.empty_retirement_claim_set(),
        );
        let first_bundle = ledger.reserve_replacement_bundle(first_spec).unwrap();
        let first_staged = renderer
            .stage_replacement(
                &device,
                &queue,
                &requests,
                first_plan,
                &first_bundle,
                &ledger,
                220,
            )
            .unwrap();
        let first_ready = renderer
            .preflight_replacement_publication(first_staged)
            .unwrap();
        let mut first_leases = first_bundle
            .commit(first_ready.actuals().to_vec())
            .unwrap()
            .settle()
            .unwrap();
        let first_publishable = renderer.bind_replacement_leases(first_ready, &mut first_leases);
        let (prior, _, first_admission) = renderer.publish_replacement(first_publishable);
        assert!(first_leases.is_empty());
        assert_eq!(first_admission.admitted_pages, 2);
        render_test_frame(&device, &queue, &renderer, &prior).unwrap();
        ledger.set_limits(0, 64 << 20, 16 << 20).unwrap();
        let replacement_requests = [request(&scene, 2, camera, lane_options(true))];
        let plan = renderer
            .plan_replacement(
                &replacement_requests,
                &ledger.snapshot().unwrap(),
                AllocationBundleSlot::new(160),
            )
            .unwrap();
        let mut prior_frames = vec![prior];
        let retirement = renderer
            .detach_replacement_retirement(&ledger, &plan, &mut prior_frames)
            .unwrap();
        assert!(prior_frames.is_empty());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        assert!(matches!(
            ledger.reserve_replacement_bundle(spec),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. })
        ));
        let restored = renderer.restore_replacement_retirement(retirement).unwrap();
        assert_eq!(restored.len(), 1);
        render_test_frame(&device, &queue, &renderer, &restored[0]).unwrap();
    }

    #[test]
    fn mandatory_candidate_uses_transition_reserve_while_prior_frame_is_pinned() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (prior, _, prior_admission) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 1, camera, lane_options(false))],
                &ledger,
                90,
            )
            .unwrap();
        assert_eq!(prior_admission.admitted_pages, 1);
        let base_budget = ledger.snapshot().unwrap().charged;
        ledger.set_budget(base_budget).unwrap();

        let (candidate, _, candidate_admission) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 2, camera, lane_options(true))],
                &ledger,
                91,
            )
            .unwrap();
        assert_eq!(candidate_admission.admitted_pages, 1);
        assert_eq!(candidate.draw_count(), 2);
        assert!(ledger.snapshot().unwrap().charged > base_budget);
        render_test_frame(&device, &queue, &renderer, &prior).unwrap();
        render_test_frame(&device, &queue, &renderer, &candidate).unwrap();
    }

    #[test]
    fn failed_aggregate_transition_keeps_the_table_retryable_after_prior_frame_retires() {
        let Some((device, queue)) = headless_device() else {
            return;
        };

        // Size the transition reserve so the missing page and the table growth each fit alone,
        // but their aggregate does not. This exercises the reservation ordering rather than a
        // trivially undersized transition reserve.
        let probe_ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let probe_scene = test_snapshot(&probe_ledger);
        let probe_requests = unique_summary_requests(
            &[request(
                &probe_scene,
                1,
                camera(probe_scene.prepared_interval()),
                lane_options(true),
            )],
            true,
        )
        .unwrap();
        let allele = probe_requests
            .iter()
            .find(|page| page.key.family() == SummaryPageFamily::Alleles)
            .unwrap()
            .borrowed();
        let packed = PackedAlignmentSummaryPage::from_request(allele).unwrap();
        let candidate_page_bytes = packed_buffer_capacity(&packed)
            .unwrap()
            .saturating_add(packed.range_index_bytes());
        let table_transition_bytes =
            summary_page_table_retained_bytes(next_summary_capacity(2).unwrap().saturating_mul(2))
                .unwrap();
        let transition_reserve = candidate_page_bytes.max(table_transition_bytes);
        assert!(candidate_page_bytes > 0 && table_transition_bytes > 0);
        drop(probe_requests);
        drop(probe_scene);
        drop(probe_ledger);

        let ledger =
            AllocationLedger::new_with_transient_budget(64 << 20, transition_reserve, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        // A wider prior frame makes the resources released at retirement large enough for the
        // exact same candidate to succeed on retry without changing the ledger budget.
        let prior_requests = (0..8)
            .map(|sequence| request(&scene, sequence, camera, lane_options(false)))
            .collect::<Vec<_>>();
        let (prior, _, _) = renderer
            .prepare_with_admission(&device, &queue, &prior_requests, &ledger, 92)
            .unwrap();
        assert_eq!(renderer.page_table_capacity, 1);
        ledger
            .set_budget(ledger.snapshot().unwrap().charged)
            .unwrap();

        let error = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 9, camera, lane_options(true))],
                &ledger,
                93,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            AlignmentSummaryGpuFrameBuildError::Admission(
                AlignmentSummaryGpuPageAdmissionError::Ledger(LedgerError::BudgetExceeded { .. })
            )
        ));
        assert_eq!(renderer.page_table_capacity, 1);
        assert_eq!(renderer.resident_page_count(), 1);
        render_test_frame(&device, &queue, &renderer, &prior).unwrap();

        drop(prior);
        let (candidate, _, admission) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 10, camera, lane_options(true))],
                &ledger,
                94,
            )
            .unwrap();
        assert_eq!(admission.admitted_pages, 1);
        assert_eq!(candidate.draw_count(), 2);
        render_test_frame(&device, &queue, &renderer, &candidate).unwrap();
    }

    #[test]
    fn idle_frame_ring_slots_are_reclaimed_after_overlapping_frames_retire() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let camera = camera(scene.prepared_interval());
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (first, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 1, camera, lane_options(false))],
                &ledger,
                100,
            )
            .unwrap();
        let (second, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 2, camera, lane_options(true))],
                &ledger,
                101,
            )
            .unwrap();
        assert_eq!(renderer.frame_buffers.len(), 2);
        drop(first);
        drop(second);

        let (_third, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(&scene, 3, camera, lane_options(false))],
                &ledger,
                102,
            )
            .unwrap();
        assert_eq!(renderer.frame_buffers.len(), 1);
    }

    #[test]
    fn resident_byte_stats_exclude_unrelated_shared_ledger_gpu_allocations() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let unrelated_bytes = 4096;
        let _unrelated = ledger
            .reserve(
                AllocationClass::GpuPage,
                unrelated_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(unrelated_bytes, 1)
            .unwrap();
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (_frame, stats, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(
                    &scene,
                    1,
                    camera(scene.prepared_interval()),
                    lane_options(false),
                )],
                &ledger,
                110,
            )
            .unwrap();
        assert_eq!(
            stats.resident_gpu_bytes,
            renderer.resident_gpu_bytes().unwrap()
        );
        let global_gpu_bytes = ledger
            .snapshot()
            .unwrap()
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::GpuPage)
            .map(|record| record.charged_capacity)
            .sum::<usize>();
        assert_eq!(
            global_gpu_bytes,
            stats.resident_gpu_bytes.saturating_add(unrelated_bytes)
        );
    }

    #[test]
    fn cooperative_reclaim_preserves_protected_pages_then_releases_unpinned_lru() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (frame, _, _) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(
                    &scene,
                    1,
                    camera(scene.prepared_interval()),
                    lane_options(false),
                )],
                &ledger,
                120,
            )
            .unwrap();
        drop(frame);
        let key = renderer.pages[0].key;
        let snapshot = ledger.snapshot().unwrap();
        let free = snapshot
            .budget
            .saturating_sub(snapshot.charged.saturating_add(snapshot.reserved));
        let required = free.saturating_add(renderer.resident_retained_bytes().unwrap());

        let protected = renderer
            .reclaim_for_headroom(&ledger, required, &[key])
            .unwrap();
        assert_eq!(protected.reclaimed_pages(), 0);
        assert_eq!(renderer.resident_page_count(), 1);
        assert!(!protected.headroom_satisfied);

        let reclaimed = renderer
            .reclaim_for_headroom(&ledger, required, &[])
            .unwrap();
        assert_eq!(reclaimed.reclaimed_pages(), 1);
        assert_eq!(renderer.resident_page_count(), 0);
        assert!(reclaimed.released_retained_bytes() > 0);
        assert!(reclaimed.released_ledger_bytes() > 0);
        assert!(reclaimed.headroom_satisfied);
    }

    #[test]
    fn cooperative_reclaim_batches_many_summary_pages_around_protected_and_pinned_members() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let page_bytes = 256;
        // The first six canonical families retain the original protected/pinned/LRU ordering.
        // The synthetic companion key is deliberately newest, so adding a supported semantic
        // family does not silently turn it into one of the two pages this fixture asks reclaim
        // to choose.
        let revisions = [60, 10, 50, 20, 40, 30, 70];
        let mut keys = scene
            .summary_pages()
            .iter()
            .map(|page| AlignmentSummaryGpuPageKey {
                semantic: page.key(),
                companion_depth: None,
            })
            .collect::<Vec<_>>();
        let extra_semantic = keys[0].semantic;
        let extra_companion = keys[1].semantic;
        keys.push(AlignmentSummaryGpuPageKey {
            semantic: extra_semantic,
            companion_depth: Some(extra_companion),
        });
        keys.sort_unstable();
        assert_eq!(keys.len(), revisions.len());
        for (key, revision) in keys.iter().copied().zip(revisions) {
            let allocation = ledger
                .reserve(
                    AllocationClass::GpuPage,
                    page_bytes,
                    ReservationKind::Normal,
                )
                .unwrap()
                .register(page_bytes, revision)
                .unwrap();
            renderer.pages.push(GpuSummaryPage {
                key,
                interval_start: 0,
                buffer: None,
                instance_count: 0,
                metadata: GpuSummaryPageMetadata::Alleles,
                allocation: Some(allocation),
                last_used_revision: revision,
            });
        }
        renderer.pages.sort_unstable_by_key(|page| page.key);
        renderer.rebuild_reclaim_index();

        let pinned_key = keys[3];
        let pin = renderer
            .page(pinned_key)
            .unwrap()
            .allocation
            .as_ref()
            .unwrap()
            .pin()
            .unwrap();
        let protected = [keys[5], keys[1], keys[5]];
        let pages_before = renderer
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>();
        let ledger_before = ledger.snapshot().unwrap();
        let infeasible_pressure = GpuBudgetPressure::from_budget_failure(
            ledger_before.budget.saturating_add(1),
            ledger_before.charged,
            ledger_before.reserved,
            ledger_before.budget,
        );
        let infeasible = renderer
            .reclaim_for_budget_pressure(&ledger, infeasible_pressure, &protected)
            .unwrap();
        assert_eq!(infeasible.reclaimed_pages(), 0);
        assert!(!infeasible.headroom_satisfied);
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.key)
                .collect::<Vec<_>>(),
            pages_before
        );

        let eligible_lru = [keys[4], keys[2]];
        let before = ledger.snapshot().unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            page_bytes.saturating_mul(eligible_lru.len()),
            before.charged,
            before.reserved,
            before.charged.saturating_add(before.reserved),
        );
        let operations_before = renderer.operation_counters();
        let report = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &protected)
            .unwrap();
        let operations_after = renderer.operation_counters();
        let remaining = renderer
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>();

        assert_eq!(report.reclaimed_pages(), eligible_lru.len());
        assert!(report.headroom_satisfied);
        assert!(eligible_lru.iter().all(|key| !remaining.contains(key)));
        assert!(remaining.contains(&keys[0]));
        assert!(protected.iter().all(|key| remaining.contains(key)));
        assert!(remaining.contains(&pinned_key));
        assert!(remaining.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            operations_after.reclaim_authority_keys_inspected
                - operations_before.reclaim_authority_keys_inspected,
            5,
            "the online index stops after protected, pinned, and sufficient LRU candidates"
        );
        assert!(
            operations_after.reclaim_authority_keys_inspected
                - operations_before.reclaim_authority_keys_inspected
                < u64::try_from(pages_before.len()).unwrap()
        );
        drop(pin);
    }

    #[test]
    fn exact_transition_pressure_does_not_reclaim_when_the_original_limit_already_fits() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let transition_reserve = 20;
        let ledger =
            AllocationLedger::new_with_transient_budget(64 << 20, transition_reserve, 16 << 20);
        let scene = test_snapshot(&ledger);
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let frame = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(
                    &scene,
                    1,
                    camera(scene.prepared_interval()),
                    lane_options(false),
                )],
                &ledger,
                120,
            )
            .unwrap()
            .0;
        drop(frame);
        let before = ledger.snapshot().unwrap();
        let base_budget = before.charged.saturating_sub(15);
        ledger.set_budget(base_budget).unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            5,
            before.charged,
            before.reserved,
            base_budget.saturating_add(transition_reserve),
        );
        let pages_before = renderer.resident_page_count();
        let retained_before = renderer.resident_retained_bytes().unwrap();
        let report = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();

        assert!(report.headroom_satisfied);
        assert_eq!(report.pressure, pressure);
        assert_eq!(report.reclaimed_pages(), 0);
        assert_eq!(report.released_retained_bytes(), 0);
        assert_eq!(renderer.resident_page_count(), pages_before);
        assert_eq!(renderer.resident_retained_bytes().unwrap(), retained_before);
        assert!(report.ledger_charged_before > base_budget);
        assert_eq!(report.ledger_charged_after, report.ledger_charged_before);
    }

    #[test]
    fn bisulfite_interpretation_is_rejected_before_generic_page_admission() {
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let mut options = lane_options(true);
        let mut alleles = options.alleles.unwrap();
        alleles.interpretation = CoverageAlleleInterpretation::Bisulfite(BisulfiteContext::Cg);
        options.alleles = Some(alleles);
        let error = unique_summary_requests(
            &[request(
                &scene,
                1,
                camera(scene.prepared_interval()),
                options,
            )],
            true,
        )
        .unwrap_err();
        assert_eq!(
            error,
            AlignmentSummaryGpuPageError::UnsupportedAlleleInterpretation(
                CoverageAlleleInterpretation::Bisulfite(BisulfiteContext::Cg)
            )
        );
    }

    #[test]
    fn only_persistent_budget_failures_request_cross_cache_reclaim() {
        let budget = LedgerError::BudgetExceeded {
            requested: 4_096,
            charged: 8_192,
            reserved: 0,
            limit: 10_000,
        };
        let admission = AlignmentSummaryGpuFrameBuildError::Admission(
            AlignmentSummaryGpuPageAdmissionError::Ledger(budget),
        );
        assert_eq!(
            admission.persistent_budget_pressure(),
            Some(GpuBudgetPressure::from_budget_failure(
                4_096, 8_192, 0, 10_000,
            ))
        );

        let nested = AlignmentSummaryGpuFrameBuildError::Prepare(
            AlignmentSummaryGpuPrepareError::Admission(
                AlignmentSummaryGpuPageAdmissionError::Ledger(budget),
            ),
        );
        assert_eq!(
            nested.persistent_budget_pressure(),
            Some(GpuBudgetPressure::from_budget_failure(
                4_096, 8_192, 0, 10_000,
            ))
        );

        let transient = AlignmentSummaryGpuFrameBuildError::Prepare(
            AlignmentSummaryGpuPrepareError::Ledger(LedgerError::TransientBudgetExceeded {
                class: TransientResourceClass::GpuUpload,
                requested: 4_096,
                transient: 8_192,
                limit: 10_000,
            }),
        );
        assert_eq!(transient.persistent_budget_pressure(), None);
        assert_eq!(
            AlignmentSummaryGpuFrameBuildError::Prepare(
                AlignmentSummaryGpuPrepareError::InvalidFrameGeometry,
            )
            .persistent_budget_pressure(),
            None
        );
    }

    #[test]
    fn coverage_frame_remains_available_when_the_reads_lane_is_collapsed() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_snapshot(&ledger);
        let mut renderer =
            AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        // This request deliberately has no read camera, row range, or read-renderer dependency.
        let (frame, stats, admission) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                &[request(
                    &scene,
                    1,
                    camera(scene.prepared_interval()),
                    lane_options(false),
                )],
                &ledger,
                80,
            )
            .unwrap();
        assert_eq!(admission.admitted_pages, 1);
        assert_eq!(stats.mandatory_pages, 1);
        assert!(stats.depth_instances > 0);
        assert_eq!(frame.draw_count(), 1);
        render_test_frame(&device, &queue, &renderer, &frame).unwrap();
    }

    fn render_test_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &AlignmentSummaryGpuRenderer,
        frame: &AlignmentSummaryGpuFrame,
    ) -> Result<(), AlignmentSummaryGpuRenderError> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("RGV alignment summary test target"),
            size: wgpu::Extent3d {
                width: 256,
                height: 96,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("RGV alignment summary test encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RGV alignment summary test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass, frame)?;
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }
}
