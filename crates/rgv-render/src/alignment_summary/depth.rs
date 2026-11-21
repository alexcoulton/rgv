//! Depth summary pages: instance layout, CPU oracle, range-maximum index and packing.
//!
//! Moved verbatim out of `alignment_summary.rs`; items that file and its tests use are
//! `pub(super)`.

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;

/// One depth run: twelve bytes (it was thirty-two, twelve of them padding). Offsets are whole
/// bases from the page start, as in `AlignmentGpuInstance`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuDepthInstance {
    start: u32,
    length: u32,
    pub depth: u32,
}

impl AlignmentSummaryGpuDepthInstance {
    /// Packs one run, refusing a span the instance cannot hold exactly. `page` only labels the
    /// error.
    // The page error type every summary packing path already returns.
    #[allow(clippy::result_large_err)]
    pub fn new(
        page: SummaryPageKey,
        start_offset: u64,
        end_offset: u64,
        depth: u32,
    ) -> Result<Self, AlignmentSummaryGpuPageError> {
        if start_offset >= end_offset {
            return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                page,
                start: start_offset,
                end: end_offset,
            });
        }
        if end_offset > crate::MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET {
            return Err(AlignmentSummaryGpuPageError::SemanticOffsetOutOfRange {
                page,
                start: start_offset,
                end: end_offset,
                limit: crate::MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET,
            });
        }
        Ok(Self {
            // Both fit: `end_offset` is at most 7e8.
            start: start_offset as u32,
            length: (end_offset - start_offset) as u32,
            depth,
        })
    }

    /// Whole bases from the page start to the run's start.
    #[must_use]
    pub const fn start_offset(&self) -> u64 {
        self.start as u64
    }

    /// Whole bases from the page start to the run's end (exclusive).
    #[must_use]
    pub const fn end_offset(&self) -> u64 {
        self.start as u64 + self.length as u64
    }

    pub(super) fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        // `[start, length]` and the depth.
        const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Uint32x2, 1 => Uint32];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[must_use]
pub fn project_alignment_summary_depth_instance(
    instance: AlignmentSummaryGpuDepthInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> Option<AlignmentSummaryGpuProjectedBounds> {
    let clip = camera.clip();
    let (x, width) = projected_horizontal_exact(
        instance.start_offset() as f64,
        instance.end_offset() as f64,
        page_start,
        camera,
        [clip.x, clip.x + clip.width],
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

#[derive(Clone, Copy, Debug)]
pub(super) struct DepthRangeEntry {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) depth: u32,
}

/// Reusable exact range-maximum metadata built once when a depth page is admitted. Queries use a
/// segment tree and inspect no semantic primitives, including for fractional viewport edges.
#[derive(Clone, Debug, Default)]
pub struct AlignmentSummaryDepthRangeIndex {
    pub(super) entries: Vec<DepthRangeEntry>,
    pub(super) maximum_tree: Vec<u32>,
    pub(super) leaf_base: usize,
}

impl AlignmentSummaryDepthRangeIndex {
    pub(super) fn build(page: &SummarySemanticPage) -> Result<Self, AlignmentSummaryGpuPageError> {
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

    pub(super) fn owned_heap_bytes(&self) -> usize {
        self.entries
            .capacity()
            .saturating_mul(size_of::<DepthRangeEntry>())
            .saturating_add(
                self.maximum_tree
                    .capacity()
                    .saturating_mul(size_of::<u32>()),
            )
    }

    pub(super) fn maximum_over_indices(&self, mut start: usize, mut end: usize) -> u32 {
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

pub(super) fn pack_depth_page(
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
        instances.push(AlignmentSummaryGpuDepthInstance::new(
            page.key(),
            primitive.start_offset,
            primitive.end_offset,
            depth,
        )?);
    }
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Depth {
        instances,
        range_maximum,
    })
}
