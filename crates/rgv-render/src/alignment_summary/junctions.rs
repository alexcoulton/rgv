//! Splice-junction summary pages: instance layout, CPU oracle, range index, page metadata and
//! packing.
//!
//! Moved verbatim out of `alignment_summary.rs`; items that file and its tests use are
//! `pub(super)`.

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AlignmentSummaryJunctionComponentToken {
    Positive = 0,
    Negative = 1,
    Unknown = 2,
    Annotation = 3,
}

/// Camera-independent splice-junction geometry. Biological extents, evidence thresholds, the
/// stable prepared-horizon row, strand component, and annotation status are uploaded once. Scale,
/// collapsed/expanded presentation, clipping, and hover/selection remain frame state.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuJunctionInstance {
    /// Signed double-single offsets from the carrier page start. `start_offset` is negative for
    /// a junction which enters the page from the left; `end_offset` may exceed the page length.
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    /// `[component depth, total depth, minimum left flank, minimum right flank]`.
    pub evidence: [u32; 4],
    /// `[expanded row, component token, annotation token, reserved]`.
    pub identity: [u32; 4],
}

impl AlignmentSummaryGpuJunctionInstance {
    pub(super) fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32x4,
            3 => Uint32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedJunction {
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub component: AlignmentSummaryJunctionComponentToken,
    pub expanded_row: u32,
    pub stroke_width: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuJunctionHit {
    pub coherence_scope: u64,
    pub lane_identity: u64,
    pub evidence: JunctionEvidence,
    pub annotation: Option<JunctionAnnotationStatus>,
    pub component: AlignmentSummaryJunctionComponentToken,
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentSummaryGpuJunctionLabel {
    pub coherence_scope: u64,
    pub lane_identity: u64,
    pub evidence: JunctionEvidence,
    pub x: f64,
    pub y: f64,
    pub text: String,
}

/// CPU oracle for the procedural junction vertex/fragment contract. It is also the geometric
/// basis for scene-native picking, so rendered curves and interaction never reconstruct different
/// arc bounds from a legacy query.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn project_alignment_summary_junction_instance(
    instance: AlignmentSummaryGpuJunctionInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: f64,
    normalization: f64,
    row_count: u32,
    minimum_depth: u32,
    minimum_flank: u32,
    display_mode: JunctionDisplayMode,
) -> Option<AlignmentSummaryGpuProjectedJunction> {
    if instance.evidence[1] < minimum_depth
        || instance.evidence[2] < minimum_flank
        || instance.evidence[3] < minimum_flank
        || !scale_maximum.is_finite()
        || scale_maximum <= 0.0
        || !normalization.is_finite()
        || normalization <= 0.0
    {
        return None;
    }
    let component = match instance.identity[1] {
        0 => AlignmentSummaryJunctionComponentToken::Positive,
        1 => AlignmentSummaryJunctionComponentToken::Negative,
        2 => AlignmentSummaryJunctionComponentToken::Unknown,
        3 if instance.identity[2] != 0 => AlignmentSummaryJunctionComponentToken::Annotation,
        _ => return None,
    };
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let row_count = match display_mode {
        JunctionDisplayMode::Collapsed => 1,
        JunctionDisplayMode::Expanded => row_count.max(1),
    };
    let row = match display_mode {
        JunctionDisplayMode::Collapsed => 0,
        JunctionDisplayMode::Expanded if instance.identity[0] < row_count => instance.identity[0],
        JunctionDisplayMode::Expanded => return None,
    };
    let band_height = camera.clip().height / f64::from(row_count);
    let baseline = camera.clip().y + (f64::from(row) + 0.5) * band_height;
    if component == AlignmentSummaryJunctionComponentToken::Annotation {
        return Some(AlignmentSummaryGpuProjectedJunction {
            bounds: AlignmentSummaryGpuProjectedBounds {
                x: x + width * 0.5 - 2.5,
                y: baseline - 2.5,
                width: 5.0,
                height: 5.0,
            },
            component,
            expanded_row: row,
            stroke_width: 0.0,
        });
    }
    if instance.evidence[0] == 0 {
        return None;
    }
    let value = f64::from(instance.evidence[0]) * normalization;
    let fraction = (value / scale_maximum).clamp(0.0, 1.0).sqrt();
    let height = (band_height * 0.45 * (0.18 + 0.82 * fraction)).max(3.0);
    let y = if component == AlignmentSummaryJunctionComponentToken::Negative {
        baseline
    } else {
        baseline - height
    };
    // The visible span above only decides whether the arc is drawn. Its geometry is the
    // junction's full, unclamped span: the shader parameterises the parabola over that span and
    // lets the clipped quad cut it, so an off-screen end stays off-screen instead of landing on
    // the viewport edge. These bounds may therefore extend beyond the camera clip.
    let (x, width) = projected_horizontal_unclamped(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    );
    Some(AlignmentSummaryGpuProjectedJunction {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y,
            width,
            height,
        },
        component,
        expanded_row: row,
        stroke_width: (1.0 + height * 0.075).clamp(1.25, 4.5),
    })
}

/// Projects a span without clamping it to the camera clip. The width keeps the shader's
/// one-point minimum.
pub(super) fn projected_horizontal_unclamped(
    start_offset: [f32; 2],
    end_offset: [f32; 2],
    page_start: u64,
    camera: CpuSummaryCamera,
) -> (f64, f64) {
    let start_offset = f64::from(start_offset[0]) + f64::from(start_offset[1]);
    let end_offset = f64::from(end_offset[0]) + f64::from(end_offset[1]);
    let clip = camera.clip();
    let x = clip.x
        + (page_start as f64 + start_offset - camera.exact_start()) / camera.bases_per_pixel();
    let right =
        clip.x + (page_start as f64 + end_offset - camera.exact_start()) / camera.bases_per_pixel();
    (x, (right - x).max(1.0))
}

/// Distance from an arc's flat edge (the top of an upward arc, the bottom of a downward one) to
/// its centre line at arc parameter `fraction`; the fragment stage's `curve_y`.
pub(super) fn junction_arc_curve_offset(fraction: f64, height: f64, stroke_width: f64) -> f64 {
    let inner_height = (height - stroke_width).max(0.0);
    let centred = fraction.clamp(0.0, 1.0) - 0.5;
    stroke_width * 0.5 + inner_height * 4.0 * centred * centred
}

/// CPU mirror of the fragment stage's stroke distance for a junction arc: the distance from a
/// point to the arc centre line measured perpendicular to the curve (first-order), so the stroke
/// keeps its thickness on steep flanks instead of thinning with the vertical distance. `fraction`
/// is the arc parameter and `local_y` the offset from the top of the arc bounds, in points.
#[cfg(test)]
pub(super) fn junction_arc_stroke_distance(
    fraction: f64,
    local_y: f64,
    width: f64,
    height: f64,
    stroke_width: f64,
    downward: bool,
) -> f64 {
    let inner_height = (height - stroke_width).max(0.0);
    let offset = junction_arc_curve_offset(fraction, height, stroke_width);
    let curve_y = if downward { height - offset } else { offset };
    let slope = 8.0 * inner_height * (fraction - 0.5) / width.max(1.0);
    (local_y - curve_y).abs() / (1.0 + slope * slope).sqrt()
}

/// CPU mirror of the fragment stage's stroke coverage (without the strand chevron).
#[cfg(test)]
pub(super) fn junction_arc_stroke_coverage(distance: f64, stroke_width: f64) -> f64 {
    let edge0 = stroke_width * 0.5;
    let t = ((distance - edge0) / 1.0).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

#[derive(Clone, Copy, Debug)]
pub struct AlignmentSummaryJunctionEntry {
    pub(super) start_offset: u64,
    pub(super) end_offset: u64,
    pub(super) evidence: JunctionEvidence,
    pub(super) expanded_row: u32,
    pub(super) annotation: Option<JunctionAnnotationStatus>,
}

impl AlignmentSummaryJunctionEntry {
    #[must_use]
    pub const fn start_offset(self) -> u64 {
        self.start_offset
    }

    #[must_use]
    pub const fn end_offset(self) -> u64 {
        self.end_offset
    }

    #[must_use]
    pub const fn evidence(self) -> JunctionEvidence {
        self.evidence
    }

    #[must_use]
    pub const fn expanded_row(self) -> u32 {
        self.expanded_row
    }

    #[must_use]
    pub const fn annotation(self) -> Option<JunctionAnnotationStatus> {
        self.annotation
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct JunctionRangeAggregate {
    pub(super) minimum_start: u64,
    pub(super) maximum_end: u64,
    pub(super) maximum_depth: u32,
    pub(super) maximum_left_flank: u32,
    pub(super) maximum_right_flank: u32,
    pub(super) maximum_expanded_row: u32,
    pub(super) populated: bool,
}

impl JunctionRangeAggregate {
    pub(super) const fn for_entry(entry: AlignmentSummaryJunctionEntry) -> Self {
        Self {
            minimum_start: entry.start_offset,
            maximum_end: entry.end_offset,
            maximum_depth: entry.evidence.depth,
            maximum_left_flank: entry.evidence.minimum_left_flank,
            maximum_right_flank: entry.evidence.minimum_right_flank,
            maximum_expanded_row: entry.expanded_row,
            populated: true,
        }
    }

    pub(super) const fn combine(left: Self, right: Self) -> Self {
        if !left.populated {
            return right;
        }
        if !right.populated {
            return left;
        }
        Self {
            minimum_start: if left.minimum_start < right.minimum_start {
                left.minimum_start
            } else {
                right.minimum_start
            },
            maximum_end: if left.maximum_end > right.maximum_end {
                left.maximum_end
            } else {
                right.maximum_end
            },
            maximum_depth: if left.maximum_depth > right.maximum_depth {
                left.maximum_depth
            } else {
                right.maximum_depth
            },
            maximum_left_flank: if left.maximum_left_flank > right.maximum_left_flank {
                left.maximum_left_flank
            } else {
                right.maximum_left_flank
            },
            maximum_right_flank: if left.maximum_right_flank > right.maximum_right_flank {
                left.maximum_right_flank
            } else {
                right.maximum_right_flank
            },
            maximum_expanded_row: if left.maximum_expanded_row > right.maximum_expanded_row {
                left.maximum_expanded_row
            } else {
                right.maximum_expanded_row
            },
            populated: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct JunctionRangeQuery {
    pub(super) maximum_depth: u32,
    pub(super) maximum_expanded_row: u32,
    pub(super) matched: usize,
    pub(super) examined_entries: usize,
}

/// Exact interval/filter aggregate built once beside packed junction geometry. Camera capture
/// traverses only tree nodes which can overlap and satisfy the requested depth/flank thresholds;
/// it never revisits the semantic primitive list.
#[derive(Clone, Debug, Default)]
pub(super) struct AlignmentSummaryJunctionRangeIndex {
    pub(super) leaf_base: usize,
    pub(super) tree: Vec<JunctionRangeAggregate>,
}

impl AlignmentSummaryJunctionRangeIndex {
    pub(super) fn build(entries: &[AlignmentSummaryJunctionEntry]) -> Self {
        if entries.is_empty() {
            return Self::default();
        }
        let leaf_base = entries.len().next_power_of_two();
        let mut tree = vec![JunctionRangeAggregate::default(); leaf_base.saturating_mul(2)];
        for (index, entry) in entries.iter().copied().enumerate() {
            tree[leaf_base + index] = JunctionRangeAggregate::for_entry(entry);
        }
        for node in (1..leaf_base).rev() {
            tree[node] = JunctionRangeAggregate::combine(tree[node * 2], tree[node * 2 + 1]);
        }
        Self { leaf_base, tree }
    }

    pub(super) fn query(
        &self,
        entries: &[AlignmentSummaryJunctionEntry],
        start: u64,
        end: u64,
        minimum_depth: u32,
        minimum_flank: u32,
    ) -> JunctionRangeQuery {
        if self.leaf_base == 0 || start >= end {
            return JunctionRangeQuery::default();
        }
        let mut result = JunctionRangeQuery::default();
        self.query_node(
            1,
            entries,
            start,
            end,
            minimum_depth,
            minimum_flank,
            &mut result,
        );
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn query_node(
        &self,
        node: usize,
        entries: &[AlignmentSummaryJunctionEntry],
        start: u64,
        end: u64,
        minimum_depth: u32,
        minimum_flank: u32,
        result: &mut JunctionRangeQuery,
    ) {
        let Some(aggregate) = self.tree.get(node).copied() else {
            return;
        };
        if !aggregate.populated
            || aggregate.minimum_start >= end
            || aggregate.maximum_end <= start
            || aggregate.maximum_depth < minimum_depth
            || aggregate.maximum_left_flank < minimum_flank
            || aggregate.maximum_right_flank < minimum_flank
        {
            return;
        }
        if node >= self.leaf_base {
            let entry_index = node - self.leaf_base;
            let Some(entry) = entries.get(entry_index).copied() else {
                return;
            };
            result.examined_entries = result.examined_entries.saturating_add(1);
            if entry.start_offset < end
                && entry.end_offset > start
                && entry.evidence.depth >= minimum_depth
                && entry.evidence.minimum_left_flank >= minimum_flank
                && entry.evidence.minimum_right_flank >= minimum_flank
            {
                result.matched = result.matched.saturating_add(1);
                result.maximum_depth = result.maximum_depth.max(entry.evidence.depth);
                result.maximum_expanded_row = result.maximum_expanded_row.max(entry.expanded_row);
            }
            return;
        }
        self.query_node(
            node * 2,
            entries,
            start,
            end,
            minimum_depth,
            minimum_flank,
            result,
        );
        self.query_node(
            node * 2 + 1,
            entries,
            start,
            end,
            minimum_depth,
            minimum_flank,
            result,
        );
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentSummaryJunctionPageMetadata {
    pub(super) entries: Vec<AlignmentSummaryJunctionEntry>,
    pub(super) range_index: AlignmentSummaryJunctionRangeIndex,
    pub(super) normalization_mapped_records: Option<u64>,
    pub(super) maximum_expanded_row: u32,
}

impl AlignmentSummaryJunctionPageMetadata {
    #[must_use]
    pub fn entries(&self) -> &[AlignmentSummaryJunctionEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn normalization_mapped_records(&self) -> Option<u64> {
        self.normalization_mapped_records
    }

    #[must_use]
    pub const fn maximum_expanded_row(&self) -> u32 {
        self.maximum_expanded_row
    }

    pub(super) fn owned_heap_bytes(&self) -> usize {
        self.entries
            .capacity()
            .saturating_mul(size_of::<AlignmentSummaryJunctionEntry>())
            .saturating_add(
                self.range_index
                    .tree
                    .capacity()
                    .saturating_mul(size_of::<JunctionRangeAggregate>()),
            )
    }

    pub(super) fn normalization_factor(&self, count_mode: SashimiCountMode) -> Option<f64> {
        match count_mode {
            SashimiCountMode::Raw => Some(1.0),
            SashimiCountMode::CountsPerMillion => self
                .normalization_mapped_records
                .filter(|total| *total > 0)
                .map(|total| 1_000_000.0 / total as f64),
        }
    }
}

pub(super) fn pack_junction_page(
    page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::junctions(page)?;
    let page_start = page.interval().start();
    let mut instances = Vec::with_capacity(page.primitives().len().saturating_mul(4));
    let mut entries = Vec::with_capacity(page.primitives().len());
    let mut maximum_expanded_row = 0_u32;
    for primitive in page.primitives() {
        if primitive.start_offset >= primitive.end_offset {
            return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                page: page.key(),
                start: primitive.start_offset,
                end: primitive.end_offset,
            });
        }
        let SummarySemanticComponent::Junction(junction) = primitive.component else {
            return Err(AlignmentSummaryGpuPageError::UnexpectedSemanticComponent(
                page.key(),
            ));
        };
        maximum_expanded_row = maximum_expanded_row.max(junction.expanded_row);
        // Geometry comes from the biological extent, not the tile-clipped primitive span: a
        // junction entering the first prepared tile starts left of the page, so its start offset
        // is negative. The primitive span stays the page-local overlap extent for range queries.
        let start_offset = junction_page_offset(junction.evidence.start, page_start);
        let end_offset = junction_page_offset(junction.evidence.end, page_start);
        entries.push(AlignmentSummaryJunctionEntry {
            start_offset: primitive.start_offset,
            end_offset: primitive.end_offset,
            evidence: junction.evidence,
            expanded_row: junction.expanded_row,
            annotation: junction.annotation,
        });
        let annotation = junction.annotation.map_or(0, |status| match status {
            JunctionAnnotationStatus::Annotated => 1,
            JunctionAnnotationStatus::Novel => 2,
            JunctionAnnotationStatus::Unknown => 3,
        });
        for (component_depth, component) in [
            (
                junction.evidence.positive_depth,
                AlignmentSummaryJunctionComponentToken::Positive,
            ),
            (
                junction.evidence.negative_depth,
                AlignmentSummaryJunctionComponentToken::Negative,
            ),
            (
                junction.evidence.unknown_depth,
                AlignmentSummaryJunctionComponentToken::Unknown,
            ),
        ] {
            if component_depth == 0 {
                continue;
            }
            instances.push(AlignmentSummaryGpuJunctionInstance {
                start_offset,
                end_offset,
                evidence: [
                    component_depth,
                    junction.evidence.depth,
                    junction.evidence.minimum_left_flank,
                    junction.evidence.minimum_right_flank,
                ],
                identity: [junction.expanded_row, component as u32, annotation, 0],
            });
        }
        if junction.annotation.is_some() {
            instances.push(AlignmentSummaryGpuJunctionInstance {
                start_offset,
                end_offset,
                evidence: [
                    0,
                    junction.evidence.depth,
                    junction.evidence.minimum_left_flank,
                    junction.evidence.minimum_right_flank,
                ],
                identity: [
                    junction.expanded_row,
                    AlignmentSummaryJunctionComponentToken::Annotation as u32,
                    annotation,
                    0,
                ],
            });
        }
    }
    instances.shrink_to_fit();
    entries.shrink_to_fit();
    let range_index = AlignmentSummaryJunctionRangeIndex::build(&entries);
    Ok(PackedAlignmentSummaryPage::Junctions {
        instances,
        metadata: AlignmentSummaryJunctionPageMetadata {
            entries,
            range_index,
            normalization_mapped_records: page.key().junction_normalization_mapped_records(),
            maximum_expanded_row,
        },
    })
}
