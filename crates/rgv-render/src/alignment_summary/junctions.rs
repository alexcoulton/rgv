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

/// Camera-independent splice-junction geometry: one arc instance per drawn junction (plus its
/// annotation marker). Biological extents, evidence thresholds, the stable prepared-horizon row,
/// the resolved strand, and annotation status are uploaded once. Scale, collapsed/expanded
/// presentation, clipping, and hover/selection remain frame state.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuJunctionInstance {
    /// Signed double-single offsets from the carrier page start. `start_offset` is negative for
    /// a junction which enters the page from the left; `end_offset` may exceed the page length.
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    /// `[depth, depth, minimum left flank, minimum right flank]`. The first slot held a strand
    /// component's share of the depth when a junction was drawn once per strand.
    pub evidence: [u32; 4],
    /// `[expanded row, resolved strand (component token), annotation token, reserved]`.
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
    /// Resolved strand, its source, motif and splice-site totals of the hit junction.
    pub presentation: rgv_model::JunctionPresentation,
    pub stroke_width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentSummaryGpuJunctionLabel {
    pub coherence_scope: u64,
    pub lane_identity: u64,
    pub evidence: JunctionEvidence,
    /// Centre of the label text.
    pub x: f64,
    pub y: f64,
    pub text: String,
    /// The label's chip (its collision box), drawn behind the text so it stays legible over
    /// other arcs.
    pub chip: [f64; 4],
}

/// Strand token drawn for a resolved junction strand.
#[must_use]
pub const fn junction_strand_component(
    strand: rgv_model::JunctionStrand,
) -> AlignmentSummaryJunctionComponentToken {
    match strand {
        rgv_model::JunctionStrand::Positive => AlignmentSummaryJunctionComponentToken::Positive,
        rgv_model::JunctionStrand::Negative => AlignmentSummaryJunctionComponentToken::Negative,
        rgv_model::JunctionStrand::Unknown => AlignmentSummaryJunctionComponentToken::Unknown,
    }
}

/// CPU oracle for the procedural junction vertex/fragment contract. It is also the geometric
/// basis for scene-native picking, so rendered curves and interaction never reconstruct different
/// arc bounds from a legacy query. Assumes labels are shown; see
/// [`project_alignment_summary_junction_instance_with_labels`].
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
    project_alignment_summary_junction_instance_with_labels(
        instance,
        page_start,
        camera,
        scale_maximum,
        normalization,
        row_count,
        minimum_depth,
        minimum_flank,
        display_mode,
        true,
    )
}

/// The junction arc's geometry, mirroring `junction_vertex` in `alignment_summary.wgsl`: the arc
/// rises (or, on the minus strand, falls) by [`rgv_model::sashimi_arc_height`] of its full
/// on-screen span, and its stroke is [`rgv_model::sashimi_stroke_width`] of its read count
/// against the lane's scale maximum. `labels` reserves room above the apex for a count label.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn project_alignment_summary_junction_instance_with_labels(
    instance: AlignmentSummaryGpuJunctionInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: f64,
    normalization: f64,
    row_count: u32,
    minimum_depth: u32,
    minimum_flank: u32,
    display_mode: JunctionDisplayMode,
    labels: bool,
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
    if instance.evidence[1] == 0 {
        return None;
    }
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
    let value = f64::from(instance.evidence[1]) * normalization;
    let stroke_width = rgv_model::sashimi_stroke_width(value, scale_maximum);
    let available = rgv_model::sashimi_available_height(band_height, labels);
    let height = rgv_model::sashimi_arc_height(width, available, stroke_width);
    let y = if component == AlignmentSummaryJunctionComponentToken::Negative {
        baseline
    } else {
        baseline - height
    };
    Some(AlignmentSummaryGpuProjectedJunction {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y,
            width,
            height,
        },
        component,
        expanded_row: row,
        stroke_width,
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

/// Distance from a point to a projected arc's centre line, measured as the fragment stage does
/// (perpendicular, first order); beyond either end of the arc, the distance to that end.
#[must_use]
pub fn junction_arc_distance(
    bounds: AlignmentSummaryGpuProjectedBounds,
    stroke_width: f64,
    downward: bool,
    x: f64,
    y: f64,
) -> Option<f64> {
    if !(bounds.width > 0.0 && bounds.height > 0.0 && x.is_finite() && y.is_finite()) {
        return None;
    }
    let fraction = (x - bounds.x) / bounds.width;
    if (0.0..=1.0).contains(&fraction) {
        return Some(junction_arc_stroke_distance(
            fraction,
            y - bounds.y,
            bounds.width,
            bounds.height,
            stroke_width,
            downward,
        ));
    }
    let end_x = if fraction < 0.0 {
        bounds.x
    } else {
        bounds.x + bounds.width
    };
    let offset = junction_arc_curve_offset(fraction, bounds.height, stroke_width);
    let end_y = if downward {
        bounds.y + bounds.height - offset
    } else {
        bounds.y + offset
    };
    Some(((x - end_x).powi(2) + (y - end_y).powi(2)).sqrt())
}

#[derive(Clone, Copy, Debug)]
pub struct AlignmentSummaryJunctionEntry {
    pub(super) start_offset: u64,
    pub(super) end_offset: u64,
    pub(super) evidence: JunctionEvidence,
    pub(super) expanded_row: u32,
    pub(super) annotation: Option<JunctionAnnotationStatus>,
    pub(super) presentation: rgv_model::JunctionPresentation,
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

    /// How this junction is drawn: resolved strand, weak/filter state, splice-site totals.
    #[must_use]
    pub const fn presentation(self) -> rgv_model::JunctionPresentation {
        self.presentation
    }

    /// The arc this junction draws as, unless it is hidden.
    #[must_use]
    pub const fn drawn_component(self) -> Option<AlignmentSummaryJunctionComponentToken> {
        if self.presentation.hidden() {
            None
        } else {
            Some(junction_strand_component(self.presentation.strand))
        }
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
        // A hidden junction neither sets the lane's scale nor is counted as drawn.
        if entry.presentation.hidden() {
            return Self {
                minimum_start: 0,
                maximum_end: 0,
                maximum_depth: 0,
                maximum_left_flank: 0,
                maximum_right_flank: 0,
                maximum_expanded_row: 0,
                populated: false,
            };
        }
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
            if !entry.presentation.hidden()
                && entry.start_offset < end
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
    presentation: Option<&rgv_model::SashimiPresentationTable>,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::junctions(page)?;
    let page_start = page.interval().start();
    let mut instances = Vec::with_capacity(page.primitives().len().saturating_mul(2));
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
        let junction_presentation = presentation.map_or_else(
            || rgv_model::JunctionPresentation::from_evidence(&junction.evidence),
            |table| table.resolve(&junction.evidence),
        );
        entries.push(AlignmentSummaryJunctionEntry {
            start_offset: primitive.start_offset,
            end_offset: primitive.end_offset,
            evidence: junction.evidence,
            expanded_row: junction.expanded_row,
            annotation: junction.annotation,
            presentation: junction_presentation,
        });
        if junction_presentation.hidden() || junction.evidence.depth == 0 {
            continue;
        }
        let annotation = junction.annotation.map_or(0, |status| match status {
            JunctionAnnotationStatus::Annotated => 1,
            JunctionAnnotationStatus::Novel => 2,
            JunctionAnnotationStatus::Unknown => 3,
        });
        // One arc per junction, on its resolved strand.
        instances.push(AlignmentSummaryGpuJunctionInstance {
            start_offset,
            end_offset,
            evidence: [
                junction.evidence.depth,
                junction.evidence.depth,
                junction.evidence.minimum_left_flank,
                junction.evidence.minimum_right_flank,
            ],
            identity: [
                junction.expanded_row,
                junction_strand_component(junction_presentation.strand) as u32,
                annotation,
                0,
            ],
        });
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
