//! Depth summary pages: instance layout, CPU oracle, range-maximum index and packing.
//!
//! Moved verbatim out of `alignment_summary.rs`; items that file and its tests use are
//! `pub(super)`.

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuDepthInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    pub depth: u32,
    pub(super) padding: [u32; 3],
}

impl AlignmentSummaryGpuDepthInstance {
    pub(super) fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
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
