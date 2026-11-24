use std::{collections::HashMap, mem::size_of, ops::Range, sync::Arc};

use rgv_core::{GenomicInterval, TrackId, Viewport};
use thiserror::Error;

use crate::{
    AlignmentComponent, AlignmentGeometryToken, AlignmentHandle, AlignmentInteractionState,
    AlignmentInteractionToken, AlignmentLabelKind, AlignmentPageFamily, AlignmentPanelLaneFrame,
    AlignmentPresentationScene, AlignmentSemanticPrimitive, AlignmentStyleToken, ClipEdgeToken,
    LedgerError, ReservationKind, SceneRevision, TransientResourceClass,
    TransientResourceReservation,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuClipRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CpuClipRect {
    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Live camera state deliberately absent from semantic page identities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentCamera {
    viewport: Viewport,
    row_scroll: f64,
    row_height: f64,
    read_height: f64,
    clip: CpuClipRect,
}

impl AlignmentCamera {
    pub fn new(
        interval: GenomicInterval,
        row_scroll: f64,
        row_height: f64,
        read_height: f64,
        clip: CpuClipRect,
    ) -> Result<Self, CpuProjectionError> {
        validate_camera_geometry(row_scroll, row_height, read_height, clip)?;
        let viewport =
            Viewport::new(interval, clip.width).map_err(|_| CpuProjectionError::InvalidCamera)?;
        Ok(Self {
            viewport,
            row_scroll,
            row_height,
            read_height,
            clip,
        })
    }

    /// Builds a camera from the application's fractional genomic viewport. This is the live-pan
    /// path: sub-base motion changes only the camera transform and never page identity.
    pub fn from_viewport(
        viewport: Viewport,
        row_scroll: f64,
        row_height: f64,
        read_height: f64,
        clip: CpuClipRect,
    ) -> Result<Self, CpuProjectionError> {
        validate_camera_geometry(row_scroll, row_height, read_height, clip)?;
        let viewport = viewport
            .resized(clip.width)
            .map_err(|_| CpuProjectionError::InvalidCamera)?;
        Ok(Self {
            viewport,
            row_scroll,
            row_height,
            read_height,
            clip,
        })
    }

    #[must_use]
    pub const fn viewport(self) -> Viewport {
        self.viewport
    }

    #[must_use]
    pub const fn interval(self) -> GenomicInterval {
        self.viewport.interval()
    }

    #[must_use]
    pub fn exact_start(self) -> f64 {
        self.viewport.exact_start()
    }

    #[must_use]
    pub fn span(self) -> f64 {
        self.viewport.span()
    }

    #[must_use]
    pub fn bases_per_pixel(self) -> f64 {
        self.viewport.bases_per_pixel()
    }

    #[must_use]
    pub const fn row_scroll(self) -> f64 {
        self.row_scroll
    }

    #[must_use]
    pub const fn row_height(self) -> f64 {
        self.row_height
    }

    #[must_use]
    pub const fn read_height(self) -> f64 {
        self.read_height
    }

    #[must_use]
    pub const fn clip(self) -> CpuClipRect {
        self.clip
    }

    #[must_use]
    pub fn visible_rows(self) -> Range<u32> {
        let first = self.row_scroll.floor().max(0.0) as u32;
        let visible = (self.clip.height / self.row_height).ceil().max(1.0) as u32;
        first..first.saturating_add(visible).saturating_add(1)
    }

    /// Absolute genomic positions, as a half-open range, outside which no primitive can project
    /// into this camera's clip, on the CPU ([`CpuAlignmentProjector`]) or on the GPU
    /// (`alignment_scene.wgsl`). A primitive whose span does not intersect it emits nothing.
    ///
    /// No shape reaches further beyond its own genomic span than the one-point minimum size, a
    /// point marker's half width ([`POINT_MARKER_MAXIMUM_WIDTH`] / 2), or a clip edge's width plus
    /// the arrowhead it may sit behind (at most [`ARROWHEAD_LENGTH_FRACTION`] of the read
    /// height). The window adds [`PROJECTION_WINDOW_MARGIN_POINTS`] plus twice the drawn read
    /// height on both sides, and two more bases for rounding, which covers all of those with room
    /// to spare, including the GPU's single-precision arithmetic and a scissor rounded outwards to
    /// whole physical pixels.
    #[must_use]
    pub fn projection_window(self) -> Range<u64> {
        let body = self.read_height.max(MINIMUM_PRIMITIVE_EXTENT);
        let margin = (PROJECTION_WINDOW_MARGIN_POINTS + 2.0 * body) * self.bases_per_pixel() + 2.0;
        let start = self.exact_start() - margin;
        let end = self.exact_start() + self.span() + margin;
        let start = if start > 0.0 { start.floor() as u64 } else { 0 };
        let end = if end > 0.0 { end.ceil() as u64 } else { 0 };
        start..end.max(start)
    }

    /// Pane y of the top of the read drawn in canonical atlas `row` (unclipped), and the drawn
    /// read height (at least one point): the vertical extent every read-height shape shares.
    #[must_use]
    pub fn read_extent(self, row: u32) -> (f64, f64) {
        (
            self.clip.y
                + (f64::from(row) - self.row_scroll) * self.row_height
                + (self.row_height - self.read_height) * 0.5,
            self.read_height.max(MINIMUM_PRIMITIVE_EXTENT),
        )
    }

    fn x_for_position(self, position: u64) -> f64 {
        self.clip.x + self.viewport.pixel_at_genomic(position)
    }

    pub(crate) fn position_for_x(self, x: f64) -> Option<u64> {
        if x < self.clip.x || x >= self.clip.x + self.clip.width {
            return None;
        }
        let position = self.viewport.genomic_at_pixel(x - self.clip.x).floor() as u64;
        Some(position.clamp(
            self.interval().start(),
            self.interval().end().saturating_sub(1),
        ))
    }

    pub(crate) fn row_for_y(self, y: f64) -> Option<u32> {
        if y < self.clip.y || y >= self.clip.y + self.clip.height {
            return None;
        }
        let row = self.row_scroll + (y - self.clip.y) / self.row_height;
        (row >= 0.0 && row <= f64::from(u32::MAX)).then_some(row.floor() as u32)
    }
}

fn validate_camera_geometry(
    row_scroll: f64,
    row_height: f64,
    read_height: f64,
    clip: CpuClipRect,
) -> Result<(), CpuProjectionError> {
    if !row_scroll.is_finite()
        || row_scroll < 0.0
        || !row_height.is_finite()
        || row_height <= 0.0
        || !read_height.is_finite()
        || read_height <= 0.0
        || read_height > row_height
        || !clip.x.is_finite()
        || !clip.y.is_finite()
        || !clip.width.is_finite()
        || clip.width <= 0.0
        || !clip.height.is_finite()
        || clip.height <= 0.0
    {
        return Err(CpuProjectionError::InvalidCamera);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuPrimitiveShape {
    ReadBody,
    EventLine,
    PointMarker,
    BaseCell,
    DirectionTerminalLeft,
    DirectionTerminalRight,
    TextLabel,
    /// A clip's cut edge (see [`clip_edge_extent`]); its `geometry` says which kind.
    ClipEdge,
    /// A point marker or clip cut edge drawn as a plain one-point hairline on its boundary,
    /// because the camera is between [`BOUNDARY_MARK_HAIRLINE_POINTS_PER_BASE`] and
    /// [`BOUNDARY_MARK_FULL_POINTS_PER_BASE`] (see [`boundary_mark_detail`]); its `geometry`
    /// says which mark. No sawtooth.
    BoundaryHairline,
    /// A soft-clipped ghost base cell, drawn translucent beneath every other read of its row.
    ClipGhostCell,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuProjectedPrimitive {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub shape: CpuPrimitiveShape,
    /// The semantic geometry token this was projected from (which clip edge, which label
    /// anchoring), so presenters and exporters need not re-derive it.
    pub geometry: AlignmentGeometryToken,
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
    pub style: AlignmentStyleToken,
    pub interaction: AlignmentInteractionToken,
    pub draw_order: u32,
    pub text: Option<Arc<str>>,
    /// Canonical atlas row. This remains stable while the row camera moves.
    pub row: u32,
    /// The read's direction arrowhead that cuts this primitive, for the direction-terminal
    /// shapes and the cells and block pieces within a head's reach of the tip
    /// ([`AlignmentGeometryToken::InsideArrowhead`]): described in unclipped pane coordinates,
    /// so a primitive clipped by the pane keeps the read's own tip and diagonal rather than
    /// growing a point at the pane edge.
    pub arrow: Option<CpuArrowhead>,
    /// For a full-size point marker ([`CpuPrimitiveShape::PointMarker`]), the I-beam it draws,
    /// unclipped: picking hits its bar and serifs, not the empty corners of its box, exactly as
    /// the GPU paints it.
    pub insertion_mark: Option<CpuInsertionMark>,
}

impl CpuProjectedPrimitive {
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        if x < self.x || x >= self.x + self.width || y < self.y || y >= self.y + self.height {
            return false;
        }
        self.insertion_mark.is_none_or(|mark| {
            insertion_mark_covers(x - mark.x, y - mark.y, mark.width, mark.height)
        }) && self.arrow.is_none_or(|arrow| arrow.covers(x, y))
    }
}

/// The single direction arrowhead at a read's true terminal (shapes 3 and 4 of `fragment_main`
/// in `rgv-render`'s `alignment_scene.wgsl`; keep in lockstep). Every primitive at the tip (the
/// aligned block, the one-base direction terminal and, at base detail, the base cell) ends on
/// the read's tip and is cut by this same diagonal, so the read's outline is one point at every
/// zoom: a primitive narrower than the head is trimmed, never re-pointed at its own scale. Cells
/// and block pieces a few bases short of the tip carry the same arrow, so nothing of the read
/// pokes out of the cut-away corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuArrowhead {
    /// Pane x of the read's tip: its right end when `points_right`, else its left end.
    pub tip_x: f64,
    /// Pane y of the read's centre line.
    pub centre_y: f64,
    /// Unclipped height of the read, in points (at least one point).
    pub height: f64,
    /// The camera's horizontal scale, in points per base, which caps the head's length.
    pub points_per_base: f64,
    pub points_right: bool,
}

impl CpuArrowhead {
    /// Length of the head along the read; see [`arrowhead_length`].
    #[must_use]
    pub fn length(self) -> f64 {
        arrowhead_length(self.height, self.points_per_base)
    }

    /// Distance of `x` from the tip, measured into the read.
    #[must_use]
    pub fn distance_from_tip(self, x: f64) -> f64 {
        if self.points_right {
            self.tip_x - x
        } else {
            x - self.tip_x
        }
    }

    /// Distance from the tip at which the arrow's outline crosses height `y`: the read covers
    /// `(x, y)` from there inwards. `None` when `y` is outside the read altogether.
    #[must_use]
    pub fn outline_distance_at(self, y: f64) -> Option<f64> {
        let half_height = self.height * 0.5;
        let offset = (y - self.centre_y).abs();
        (offset <= half_height).then(|| self.length() * offset / half_height)
    }

    /// Whether the arrowhead leaves `(x, y)` painted (any point inside the read's rectangle
    /// beyond the head is).
    #[must_use]
    pub fn covers(self, x: f64, y: f64) -> bool {
        arrowhead_covers(
            self.distance_from_tip(x),
            y - self.centre_y,
            self.height,
            self.points_per_base,
        )
    }
}

/// The direction arrowhead's length along the read, for a read drawn `height` points tall
/// under a camera of `points_per_base`: [`ARROWHEAD_LENGTH_FRACTION`] of the height, but at
/// most [`ARROWHEAD_MAXIMUM_BASES`] bases, so zoomed out the head shrinks with the read instead
/// of swallowing it. A head shorter than [`ARROWHEAD_MINIMUM_LENGTH`] (under one physical pixel
/// at a 2x scale factor) is not drawn at all: the length is zero and the read ends square.
///
/// It depends on camera-global quantities only, never on the width of the primitive being cut,
/// so the block, the one-base terminal and a base cell at the tip share one diagonal; a read
/// shorter on screen than this is drawn as (part of) the arrow. Mirrors `arrowhead_length` in
/// `rgv-render`'s `alignment_scene.wgsl`.
#[must_use]
pub fn arrowhead_length(height: f64, points_per_base: f64) -> f64 {
    let length =
        (height * ARROWHEAD_LENGTH_FRACTION).min(points_per_base * ARROWHEAD_MAXIMUM_BASES);
    if length < ARROWHEAD_MINIMUM_LENGTH {
        0.0
    } else {
        length
    }
}

/// How point markers and clip cut edges are drawn at a camera scale (keep in lockstep with
/// `boundary_mark_detail` in `rgv-render`'s `alignment_scene.wgsl`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryMarkDetail {
    /// From [`BOUNDARY_MARK_FULL_POINTS_PER_BASE`]: the full marker bar, sawtooth or clip bar.
    Full,
    /// From [`BOUNDARY_MARK_HAIRLINE_POINTS_PER_BASE`]: a plain hairline
    /// [`BOUNDARY_MARK_HAIRLINE_WIDTH`] wide at the same boundary position.
    Hairline,
    /// Coarser still: not drawn, and not picked.
    Hidden,
}

/// Points per base from which point markers and clip cut edges are drawn at full size.
pub const BOUNDARY_MARK_FULL_POINTS_PER_BASE: f64 = 1.0;
/// Points per base from which they are drawn at all (as hairlines below the full scale).
pub const BOUNDARY_MARK_HAIRLINE_POINTS_PER_BASE: f64 = 0.1;
/// Width, in points, of a hairline mark (the minimum-size policy's one point).
pub const BOUNDARY_MARK_HAIRLINE_WIDTH: f64 = 1.0;
/// Points per base from which soft-clip ghost cells are drawn: the base-letter glyph width, so a
/// ghost cell appears only together with its lowercase letter, never as a pale letterless block.
pub const CLIP_GHOST_MINIMUM_POINTS_PER_BASE: f64 = 7.0;

/// Whether soft-clip ghost cells are drawn (and picked) under a camera of `points_per_base`.
#[must_use]
pub fn clip_ghost_visible(points_per_base: f64) -> bool {
    points_per_base >= CLIP_GHOST_MINIMUM_POINTS_PER_BASE
}

/// How point markers and clip cut edges are drawn under a camera of `points_per_base`.
#[must_use]
pub fn boundary_mark_detail(points_per_base: f64) -> BoundaryMarkDetail {
    if points_per_base >= BOUNDARY_MARK_FULL_POINTS_PER_BASE {
        BoundaryMarkDetail::Full
    } else if points_per_base >= BOUNDARY_MARK_HAIRLINE_POINTS_PER_BASE {
        BoundaryMarkDetail::Hairline
    } else {
        BoundaryMarkDetail::Hidden
    }
}

/// Whether a point `distance_from_tip` into the read from its tip and `offset_from_centre`
/// from its centre line is inside the arrow outline of a read `height` points tall: within the
/// head the half-height allowed grows linearly from zero at the tip to the full half-height at
/// [`arrowhead_length`] (zero: no head, the read ends square). Mirrors `fragment_main` in
/// `alignment_scene.wgsl`.
#[must_use]
pub fn arrowhead_covers(
    distance_from_tip: f64,
    offset_from_centre: f64,
    height: f64,
    points_per_base: f64,
) -> bool {
    let length = arrowhead_length(height, points_per_base);
    if distance_from_tip >= length || length <= f64::EPSILON {
        return true;
    }
    let half_height = height * 0.5;
    let allowed = half_height * (distance_from_tip / length).clamp(0.0, 1.0);
    offset_from_centre.abs() <= allowed
}

/// Horizontal extent `(left, width)` of a direction-terminal shape whose span projects to
/// `left..right`: at least one point wide (the minimum-size policy), grown away from its tip so
/// the tip stays on the read's end (leftwards when `points_right`, rightwards otherwise).
/// Mirrors `project_instance` in `alignment_scene.wgsl`.
#[must_use]
pub fn directional_extent(left: f64, right: f64, points_right: bool) -> (f64, f64) {
    let width = (right - left).max(MINIMUM_PRIMITIVE_EXTENT);
    if points_right {
        (right - width, width)
    } else {
        (left, width)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuFrameCoverage {
    pub requested_interval: GenomicInterval,
    pub covered_interval: Option<GenomicInterval>,
    pub requested_rows: Range<u32>,
    pub covered_rows: Range<u32>,
}

#[derive(Clone, Debug)]
pub struct CpuAlignmentFrame {
    pub scene_revision: SceneRevision,
    pub camera_sequence: u64,
    /// Exact camera used to project this immutable frame. Picking consumes this captured camera
    /// rather than a separately supplied mutable viewport.
    pub camera: AlignmentCamera,
    pub coverage: CpuFrameCoverage,
    pub primitives: Arc<[CpuProjectedPrimitive]>,
    _resources: Arc<CpuFrameResources>,
}

impl PartialEq for CpuAlignmentFrame {
    fn eq(&self, other: &Self) -> bool {
        self.scene_revision == other.scene_revision
            && self.camera_sequence == other.camera_sequence
            && self.camera == other.camera
            && self.coverage == other.coverage
            && self.primitives == other.primitives
    }
}

/// Bounded screen-space text projection for a persistent geometry scene. Text shaping and
/// collision remain camera-dependent, so labels are intentionally rebuilt on camera movement;
/// immutable read geometry is not. This frame contains only accepted labels and can be consumed
/// by either the software presenter or a GPU text atlas without constructing a geometry display
/// list.
#[derive(Clone, Debug)]
pub struct CpuAlignmentLabelFrame {
    pub scene_revision: SceneRevision,
    pub camera_sequence: u64,
    pub camera: AlignmentCamera,
    pub coverage: CpuFrameCoverage,
    pub labels: Arc<[CpuProjectedPrimitive]>,
    /// Immutable page-local anchors examined for this camera. This is deliberately bounded by
    /// intersecting genomic tiles and row blocks rather than the complete retained scene.
    pub anchors_examined: usize,
    /// Base-cell anchors examined before applying the base-glyph legibility gate.
    /// This lets diagnostics distinguish "no base evidence" from "base evidence was present but
    /// deliberately too small to label" without inspecting sequence or rebuilding geometry.
    pub base_anchors_examined: usize,
    /// Base labels accepted by the camera-dependent projector and handed to the presenter.
    pub base_labels_projected: usize,
    _resources: Arc<CpuFrameResources>,
}

impl PartialEq for CpuAlignmentLabelFrame {
    fn eq(&self, other: &Self) -> bool {
        self.scene_revision == other.scene_revision
            && self.camera_sequence == other.camera_sequence
            && self.camera == other.camera
            && self.coverage == other.coverage
            && self.labels == other.labels
            && self.anchors_examined == other.anchors_examined
            && self.base_anchors_examined == other.base_anchors_examined
            && self.base_labels_projected == other.base_labels_projected
    }
}

#[derive(Debug)]
struct CpuFrameResources {
    _projection: TransientResourceReservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentHit {
    pub track: TrackId,
    pub scene_revision: SceneRevision,
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
}

#[derive(Debug, Default)]
pub struct CpuAlignmentProjector;

impl CpuAlignmentProjector {
    pub fn project(
        scene: &Arc<AlignmentPresentationScene>,
        camera_sequence: u64,
        camera: AlignmentCamera,
    ) -> Result<CpuAlignmentFrame, CpuProjectionError> {
        Self::project_with_interaction(
            scene,
            camera_sequence,
            camera,
            &AlignmentInteractionState::default(),
        )
    }

    #[must_use]
    pub fn project_with_interaction(
        scene: &Arc<AlignmentPresentationScene>,
        camera_sequence: u64,
        camera: AlignmentCamera,
        interaction: &AlignmentInteractionState,
    ) -> Result<CpuAlignmentFrame, CpuProjectionError> {
        let requested_rows = camera.visible_rows();
        let covered_interval = intersect_intervals(camera.interval(), scene.prepared_interval());
        let prepared_rows = scene.prepared_rows();
        let covered_rows = intersect_ranges(requested_rows.clone(), prepared_rows);
        let visible_pages = covered_interval.map_or_else(Vec::new, |interval| {
            scene.visible_pages(interval, covered_rows.clone())
        });
        let (maximum_primitives, maximum_labels) =
            visible_pages
                .iter()
                .fold((0_usize, 0_usize), |(primitives, labels), page| {
                    (
                        primitives.saturating_add(page.primitives().len()),
                        labels.saturating_add(page.label_anchor_count()),
                    )
                });
        let mut projection_resources = scene.metadata_allocation().ledger().reserve_transient(
            TransientResourceClass::CpuProjection,
            cpu_projection_scratch_bytes(maximum_primitives, maximum_labels),
            ReservationKind::Normal,
        )?;
        let mut primitives = Vec::with_capacity(maximum_primitives);
        let mut labels =
            HashMap::<(AlignmentHandle, AlignmentComponent), CpuProjectedPrimitive>::with_capacity(
                maximum_labels,
            );
        if covered_interval.is_some() {
            for page in visible_pages {
                for primitive in page.primitives() {
                    let global_row = page.row_range().start.saturating_add(primitive.row_offset);
                    if global_row < covered_rows.start || global_row >= covered_rows.end {
                        continue;
                    }
                    if let Some(projected) = project_primitive(
                        *primitive,
                        page.interval().start(),
                        global_row,
                        camera,
                        match primitive.component {
                            AlignmentComponent::Label { id, .. } => page.label_arc(id),
                            _ => None,
                        },
                        interaction.token(primitive.handle),
                    ) {
                        if projected.shape == CpuPrimitiveShape::TextLabel {
                            labels
                                .entry((projected.handle, projected.component))
                                .and_modify(|existing| merge_label_fragments(existing, &projected))
                                .or_insert(projected);
                        } else {
                            primitives.push(projected);
                        }
                    }
                }
            }
        }
        let mut occupancy = RowOccupancy::default();
        for primitive in &primitives {
            occupancy.add(primitive);
        }
        reconcile_labels(&mut primitives, labels.into_values(), camera, &occupancy);
        // Paint order, as on the GPU: clip ghosts lie beneath the rest of their row, and boundary
        // marks (point markers, clip edges) overhang the neighbouring cells, so they follow it.
        primitives.sort_by(|left, right| {
            left.row
                .cmp(&right.row)
                .then_with(|| {
                    crate::semantic_page::primitive_paint_layer(left.component, left.geometry).cmp(
                        &crate::semantic_page::primitive_paint_layer(
                            right.component,
                            right.geometry,
                        ),
                    )
                })
                .then_with(|| left.y.total_cmp(&right.y))
                .then_with(|| left.x.total_cmp(&right.x))
                .then_with(|| left.draw_order.cmp(&right.draw_order))
                .then_with(|| left.handle.cmp(&right.handle))
                .then_with(|| left.component.cmp(&right.component))
        });
        primitives.shrink_to_fit();
        let retained_bytes = primitives
            .capacity()
            .saturating_mul(size_of::<CpuProjectedPrimitive>());
        projection_resources.shrink_to(retained_bytes)?;
        let primitives = primitives.into();
        Ok(CpuAlignmentFrame {
            scene_revision: scene.revision(),
            camera_sequence,
            camera,
            coverage: CpuFrameCoverage {
                requested_interval: camera.interval(),
                covered_interval,
                requested_rows,
                covered_rows,
            },
            primitives,
            _resources: Arc::new(CpuFrameResources {
                _projection: projection_resources,
            }),
        })
    }

    pub fn project_labels(
        scene: &Arc<AlignmentPresentationScene>,
        camera_sequence: u64,
        camera: AlignmentCamera,
    ) -> Result<CpuAlignmentLabelFrame, CpuProjectionError> {
        Self::project_labels_with_interaction(
            scene,
            camera_sequence,
            camera,
            &AlignmentInteractionState::default(),
        )
    }

    /// Projects only camera-dependent text. The scan is bounded to the visible rows of the
    /// intersecting semantic pages and, through each page's immutable spatial directory, to the
    /// primitives whose spans reach the camera's [`AlignmentCamera::projection_window`]: its cost
    /// follows what is on screen, not how much the pages hold. When the drawn read is too short
    /// for any text no anchor is projected at all. Its exact temporary capacity is admitted
    /// through the scene ledger before the projection allocates.
    ///
    /// `anchors_examined` and `base_anchors_examined` count the text-bearing primitives (labels,
    /// base cells, clip-ghost bases) of the visible rows inside that window, whether or not the
    /// camera can make them legible.
    pub fn project_labels_with_interaction(
        scene: &Arc<AlignmentPresentationScene>,
        camera_sequence: u64,
        camera: AlignmentCamera,
        interaction: &AlignmentInteractionState,
    ) -> Result<CpuAlignmentLabelFrame, CpuProjectionError> {
        let requested_rows = camera.visible_rows();
        let covered_interval = intersect_intervals(camera.interval(), scene.prepared_interval());
        let covered_rows = intersect_ranges(requested_rows.clone(), scene.prepared_rows());
        let visible_text_pages = covered_interval.map_or_else(Vec::new, |interval| {
            scene
                .visible_pages(interval, covered_rows.clone())
                .into_iter()
                .filter(|page| {
                    matches!(
                        page.key().family(),
                        AlignmentPageFamily::Labels | AlignmentPageFamily::ReadGeometry
                    )
                })
                .collect::<Vec<_>>()
        });
        // Every primitive that can project into the clip lies in these index ranges.
        let window = camera.projection_window();
        let mut ranges = Vec::new();
        let mut page_ranges = Vec::with_capacity(visible_text_pages.len());
        for page in &visible_text_pages {
            let first = ranges.len();
            page.window_primitive_ranges(covered_rows.clone(), window.clone(), &mut ranges);
            page_ranges.push(first..ranges.len());
        }
        let windowed = |page_index: usize| {
            let page = visible_text_pages[page_index];
            ranges[page_ranges[page_index].clone()]
                .iter()
                .flat_map(move |range| &page.primitives()[range.start as usize..range.end as usize])
                .copied()
        };
        let (maximum_labels, base_anchors_examined) = (0..visible_text_pages.len())
            .flat_map(windowed)
            .fold((0_usize, 0_usize), |(labels, bases), primitive| {
                match primitive.component {
                    AlignmentComponent::Base { .. } => (labels + 1, bases + 1),
                    // Clip-ghost bases also carry a letter but are not counted as base anchors.
                    AlignmentComponent::Label { .. } | AlignmentComponent::ClipGhostBase { .. } => {
                        (labels + 1, bases)
                    }
                    _ => (labels, bases),
                }
            });
        let mut projection_resources = scene.metadata_allocation().ledger().reserve_transient(
            TransientResourceClass::CpuProjection,
            cpu_label_projection_scratch_bytes(maximum_labels)
                .saturating_add(ranges.len().saturating_mul(size_of::<Range<u32>>())),
            ReservationKind::Normal,
        )?;
        let mut labels = Vec::new();
        // No label, base letter or insertion chip is drawn on a read shorter than this, and a
        // projected anchor is never taller than the drawn read: nothing to project.
        if camera.read_height.max(MINIMUM_PRIMITIVE_EXTENT) >= LABEL_MINIMUM_TEXT_HEIGHT {
            let points_per_base = 1.0 / camera.bases_per_pixel();
            let mut fragments = HashMap::<
                (AlignmentHandle, AlignmentComponent),
                CpuProjectedPrimitive,
            >::with_capacity(maximum_labels);
            for (page_index, page) in visible_text_pages.iter().enumerate() {
                for primitive in windowed(page_index).filter(|primitive| {
                    matches!(
                        primitive.component,
                        AlignmentComponent::Label { .. }
                            | AlignmentComponent::Base { .. }
                            | AlignmentComponent::ClipGhostBase {
                                obscured: false,
                                ..
                            }
                    ) && !matches!(
                        // A short insertion never has a chip (see `place_insertion_label`).
                        primitive.component,
                        AlignmentComponent::Label {
                            kind: AlignmentLabelKind::ShortInsertion,
                            ..
                        }
                    )
                }) {
                    if base_glyph_cannot_fit(primitive, points_per_base) {
                        continue;
                    }
                    let global_row = page.row_range().start.saturating_add(primitive.row_offset);
                    let text = match primitive.component {
                        AlignmentComponent::Label { id, .. } => page.label_arc(id),
                        AlignmentComponent::Base { base, .. } => Some(Arc::<str>::from(
                            char::from(base.to_ascii_uppercase()).to_string(),
                        )),
                        // Clipped bases are not part of the alignment: lowercase marks them.
                        AlignmentComponent::ClipGhostBase { base, .. } => Some(Arc::<str>::from(
                            char::from(base.to_ascii_lowercase()).to_string(),
                        )),
                        _ => unreachable!("the iterator retains only text-bearing primitives"),
                    };
                    if let Some(mut projected) = project_primitive(
                        primitive,
                        page.interval().start(),
                        global_row,
                        camera,
                        text,
                        interaction.token(primitive.handle),
                    ) {
                        // The same genomic base cell remains GPU geometry. This is a second,
                        // camera-bound text presentation of its already-retained identity.
                        projected.shape = CpuPrimitiveShape::TextLabel;
                        fragments
                            .entry((projected.handle, projected.component))
                            .and_modify(|existing| merge_label_fragments(existing, &projected))
                            .or_insert(projected);
                    }
                }
            }
            // What occupies a row is consulted only by labels placed beside a read (insertion
            // chips and clip lengths), and only in the rows that hold one.
            let beside_read_rows = fragments
                .values()
                .filter(|fragment| {
                    is_insertion_label(fragment.component)
                        || matches!(fragment.geometry, AlignmentGeometryToken::ClipEdge(_))
                })
                .map(|fragment| fragment.row)
                .collect::<std::collections::HashSet<_>>();
            let mut occupancy = RowOccupancy::default();
            if !beside_read_rows.is_empty() {
                // Read bodies and events, including modification and other specialist cells,
                // which live on overlay pages of their own.
                let mut occupant_ranges = Vec::new();
                for page in occupancy_pages(scene, covered_interval, covered_rows.clone()) {
                    occupant_ranges.clear();
                    page.window_primitive_ranges(
                        covered_rows.clone(),
                        window.clone(),
                        &mut occupant_ranges,
                    );
                    for primitive in occupant_ranges
                        .iter()
                        .flat_map(|range| {
                            &page.primitives()[range.start as usize..range.end as usize]
                        })
                        .copied()
                        .filter(|primitive| {
                            is_read_body(primitive.component)
                                || is_chip_obstacle(primitive.component)
                        })
                    {
                        let global_row =
                            page.row_range().start.saturating_add(primitive.row_offset);
                        if !beside_read_rows.contains(&global_row) {
                            continue;
                        }
                        if let Some(projected) = project_primitive(
                            primitive,
                            page.interval().start(),
                            global_row,
                            camera,
                            None,
                            AlignmentInteractionToken::default(),
                        ) {
                            occupancy.add(&projected);
                        }
                    }
                }
            }
            labels.reserve_exact(fragments.len());
            reconcile_labels(&mut labels, fragments.into_values(), camera, &occupancy);
        }
        drop(ranges);
        let base_labels_projected = labels
            .iter()
            .filter(|label| matches!(label.component, AlignmentComponent::Base { .. }))
            .count();
        labels.sort_by(|left, right| {
            left.row
                .cmp(&right.row)
                .then_with(|| left.y.total_cmp(&right.y))
                .then_with(|| left.x.total_cmp(&right.x))
                .then_with(|| left.draw_order.cmp(&right.draw_order))
                .then_with(|| left.handle.cmp(&right.handle))
                .then_with(|| left.component.cmp(&right.component))
        });
        labels.shrink_to_fit();
        projection_resources.shrink_to(
            labels
                .capacity()
                .saturating_mul(size_of::<CpuProjectedPrimitive>()),
        )?;
        Ok(CpuAlignmentLabelFrame {
            scene_revision: scene.revision(),
            camera_sequence,
            camera,
            coverage: CpuFrameCoverage {
                requested_interval: camera.interval(),
                covered_interval,
                requested_rows,
                covered_rows,
            },
            labels: labels.into(),
            anchors_examined: maximum_labels,
            base_anchors_examined,
            base_labels_projected,
            _resources: Arc::new(CpuFrameResources {
                _projection: projection_resources,
            }),
        })
    }

    /// The label projection as it was before pages carried a spatial directory: every
    /// text-bearing primitive of every visible page is projected. Kept as the reference the
    /// windowed projector must reproduce exactly.
    #[cfg(test)]
    #[allow(clippy::too_many_lines)]
    pub(crate) fn project_labels_unindexed(
        scene: &Arc<AlignmentPresentationScene>,
        camera_sequence: u64,
        camera: AlignmentCamera,
        interaction: &AlignmentInteractionState,
    ) -> Result<CpuAlignmentLabelFrame, CpuProjectionError> {
        let requested_rows = camera.visible_rows();
        let covered_interval = intersect_intervals(camera.interval(), scene.prepared_interval());
        let covered_rows = intersect_ranges(requested_rows.clone(), scene.prepared_rows());
        let visible_text_pages = covered_interval.map_or_else(Vec::new, |interval| {
            scene
                .visible_pages(interval, covered_rows.clone())
                .into_iter()
                .filter(|page| {
                    matches!(
                        page.key().family(),
                        AlignmentPageFamily::Labels | AlignmentPageFamily::ReadGeometry
                    )
                })
                .collect::<Vec<_>>()
        });
        let (maximum_labels, base_anchors_examined) =
            visible_text_pages
                .iter()
                .fold((0_usize, 0_usize), |(all_labels, all_bases), page| {
                    // Aligned base cells, and soft-clip ghost bases (which also carry a letter
                    // but are not counted as base anchors).
                    let (bases, ghosts) = page.primitives().iter().fold(
                        (0_usize, 0_usize),
                        |(bases, ghosts), primitive| match primitive.component {
                            AlignmentComponent::Base { .. } => (bases + 1, ghosts),
                            AlignmentComponent::ClipGhostBase { .. } => (bases, ghosts + 1),
                            _ => (bases, ghosts),
                        },
                    );
                    (
                        all_labels
                            .saturating_add(page.label_anchor_count())
                            .saturating_add(bases)
                            .saturating_add(ghosts),
                        all_bases.saturating_add(bases),
                    )
                });
        let mut projection_resources = scene.metadata_allocation().ledger().reserve_transient(
            TransientResourceClass::CpuProjection,
            cpu_label_projection_scratch_bytes(maximum_labels),
            ReservationKind::Normal,
        )?;
        let mut fragments =
            HashMap::<(AlignmentHandle, AlignmentComponent), CpuProjectedPrimitive>::with_capacity(
                maximum_labels,
            );
        let mut occupancy = RowOccupancy::default();
        if covered_interval.is_some() {
            for page in occupancy_pages(scene, covered_interval, covered_rows.clone()) {
                for primitive in page.primitives().iter().copied().filter(|primitive| {
                    is_read_body(primitive.component) || is_chip_obstacle(primitive.component)
                }) {
                    let global_row = page.row_range().start.saturating_add(primitive.row_offset);
                    if global_row < covered_rows.start || global_row >= covered_rows.end {
                        continue;
                    }
                    if let Some(projected) = project_primitive(
                        primitive,
                        page.interval().start(),
                        global_row,
                        camera,
                        None,
                        AlignmentInteractionToken::default(),
                    ) {
                        occupancy.add(&projected);
                    }
                }
            }
            for page in visible_text_pages {
                for primitive in page.primitives().iter().copied().filter(|primitive| {
                    matches!(
                        primitive.component,
                        AlignmentComponent::Label { .. }
                            | AlignmentComponent::Base { .. }
                            | AlignmentComponent::ClipGhostBase {
                                obscured: false,
                                ..
                            }
                    )
                }) {
                    let global_row = page.row_range().start.saturating_add(primitive.row_offset);
                    if global_row < covered_rows.start || global_row >= covered_rows.end {
                        continue;
                    }
                    let text = match primitive.component {
                        AlignmentComponent::Label { id, .. } => page.label_arc(id),
                        AlignmentComponent::Base { base, .. } => Some(Arc::<str>::from(
                            char::from(base.to_ascii_uppercase()).to_string(),
                        )),
                        // Clipped bases are not part of the alignment: lowercase marks them.
                        AlignmentComponent::ClipGhostBase { base, .. } => Some(Arc::<str>::from(
                            char::from(base.to_ascii_lowercase()).to_string(),
                        )),
                        _ => unreachable!("the iterator retains only text-bearing primitives"),
                    };
                    if let Some(mut projected) = project_primitive(
                        primitive,
                        page.interval().start(),
                        global_row,
                        camera,
                        text,
                        interaction.token(primitive.handle),
                    ) {
                        // The same genomic base cell remains GPU geometry. This is a second,
                        // camera-bound text presentation of its already-retained identity.
                        projected.shape = CpuPrimitiveShape::TextLabel;
                        fragments
                            .entry((projected.handle, projected.component))
                            .and_modify(|existing| merge_label_fragments(existing, &projected))
                            .or_insert(projected);
                    }
                }
            }
        }
        let mut labels = Vec::with_capacity(maximum_labels);
        reconcile_labels(&mut labels, fragments.into_values(), camera, &occupancy);
        let base_labels_projected = labels
            .iter()
            .filter(|label| matches!(label.component, AlignmentComponent::Base { .. }))
            .count();
        labels.sort_by(|left, right| {
            left.row
                .cmp(&right.row)
                .then_with(|| left.y.total_cmp(&right.y))
                .then_with(|| left.x.total_cmp(&right.x))
                .then_with(|| left.draw_order.cmp(&right.draw_order))
                .then_with(|| left.handle.cmp(&right.handle))
                .then_with(|| left.component.cmp(&right.component))
        });
        labels.shrink_to_fit();
        projection_resources.shrink_to(
            labels
                .capacity()
                .saturating_mul(size_of::<CpuProjectedPrimitive>()),
        )?;
        Ok(CpuAlignmentLabelFrame {
            scene_revision: scene.revision(),
            camera_sequence,
            camera,
            coverage: CpuFrameCoverage {
                requested_interval: camera.interval(),
                covered_interval,
                requested_rows,
                covered_rows,
            },
            labels: labels.into(),
            anchors_examined: maximum_labels,
            base_anchors_examined,
            base_labels_projected,
            _resources: Arc::new(CpuFrameResources {
                _projection: projection_resources,
            }),
        })
    }

    /// Resolves interaction through the exact immutable scene/frame capture that produced the
    /// visible pixels. A stale frame is refused explicitly instead of selecting from a newer
    /// scene with old camera geometry.
    pub fn pick(
        scene: &Arc<AlignmentPresentationScene>,
        track: TrackId,
        frame: &CpuAlignmentFrame,
        x: f64,
        y: f64,
    ) -> Result<Option<AlignmentHit>, CpuInteractionError> {
        if frame.scene_revision != scene.revision() {
            return Err(CpuInteractionError::StaleSceneFrame {
                frame: frame.scene_revision,
                scene: scene.revision(),
            });
        }
        Ok(pick_captured_scene(
            scene,
            track,
            frame.camera,
            frame.coverage.covered_interval,
            frame.coverage.covered_rows.clone(),
            x,
            y,
        ))
    }

    /// Picks directly from the immutable panel-lane capture consumed by the GPU renderer. This
    /// performs no screen-list projection and cannot observe a camera or snapshot other than the
    /// one that produced the visible frame.
    pub fn pick_panel_lane(
        lane: &AlignmentPanelLaneFrame,
        x: f64,
        y: f64,
    ) -> Result<Option<AlignmentHit>, CpuInteractionError> {
        let Some(snapshot) = &lane.snapshot else {
            return Ok(None);
        };
        let Some(camera) = lane.read_camera else {
            return Ok(None);
        };
        Ok(pick_captured_scene(
            snapshot.reads(),
            lane.track_id,
            camera,
            lane.covered_interval,
            lane.covered_rows.clone(),
            x,
            y,
        ))
    }
}

fn pick_captured_scene(
    scene: &Arc<AlignmentPresentationScene>,
    track: TrackId,
    camera: AlignmentCamera,
    covered_interval: Option<GenomicInterval>,
    covered_rows: Range<u32>,
    x: f64,
    y: f64,
) -> Option<AlignmentHit> {
    if !camera.clip.contains(x, y) {
        return None;
    }
    let Some(position) = camera.position_for_x(x) else {
        return None;
    };
    let Some(row) = camera.row_for_y(y) else {
        return None;
    };
    if !covered_interval.is_some_and(|interval| interval.contains(position))
        || row < covered_rows.start
        || row >= covered_rows.end
    {
        return None;
    }
    // A point marker (an I-beam) is centred on a base boundary and overhangs by at most half
    // its maximum width, and a clip edge lies at most an arrowhead plus its own width inside its boundary,
    // so marks whose one-base span lies a cell (plus that reach) away from the pointer's base
    // can still be under it.
    let covered_interval = covered_interval?;
    let pointer = camera.viewport.genomic_at_pixel(x - camera.clip.x);
    let reach = (POINT_MARKER_MAXIMUM_WIDTH * 0.5).max(
        SOFT_CLIP_EDGE_MAXIMUM_WIDTH + camera.read_height.max(MINIMUM_PRIMITIVE_EXTENT) * 0.55,
    ) * camera.bases_per_pixel();
    let marker_start = ((pointer - reach).floor() - 1.0).max(0.0) as u64;
    let marker_end = ((pointer + reach).floor() as u64).saturating_add(2);
    let search_interval = GenomicInterval::new(
        camera.interval().contig(),
        marker_start.max(covered_interval.start()).min(position),
        marker_end
            .min(covered_interval.end())
            .max(position.saturating_add(1)),
    )
    .ok()?;
    // The page spatial index narrows the row to primitives intersecting the search window; the
    // candidate rule (the base under the pointer, or a boundary mark within reach) is applied
    // exactly as before.
    let window = marker_start.min(position)..marker_end.max(position.saturating_add(1));
    let mut best: Option<CpuProjectedPrimitive> = None;
    for_each_row_window_primitive(
        scene,
        search_interval,
        row,
        window,
        |page_start, primitive| {
            if matches!(primitive.component, AlignmentComponent::Label { .. }) {
                return;
            }
            let start = page_start.saturating_add(primitive.start_offset);
            let end = page_start.saturating_add(primitive.end_offset);
            let candidate = (start <= position && position < end)
                || (primitive.geometry.is_boundary_mark()
                    && start < marker_end
                    && marker_start < end);
            if !candidate {
                return;
            }
            let Some(projected) = project_primitive(
                primitive,
                page_start,
                row,
                camera,
                None,
                AlignmentInteractionToken::default(),
            ) else {
                return;
            };
            // Boundary marks paint over their row and clip ghosts beneath it (see the semantic page
            // order), so the paint layer decides here too.
            if projected.contains(x, y)
                && best
                    .as_ref()
                    .is_none_or(|best| pick_rank(&projected) > pick_rank(best))
            {
                best = Some(projected);
            }
        },
    );
    best.map(|primitive| AlignmentHit {
        track,
        scene_revision: scene.revision(),
        handle: primitive.handle,
        component: primitive.component,
    })
}

/// Paint order of a picked primitive, topmost greatest.
fn pick_rank(primitive: &CpuProjectedPrimitive) -> (u8, u32, AlignmentComponent, AlignmentHandle) {
    (
        crate::semantic_page::primitive_paint_layer(primitive.component, primitive.geometry),
        primitive.draw_order,
        primitive.component,
        primitive.handle,
    )
}

/// Calls `visit` with the page start and every primitive of canonical `row` whose genomic span
/// intersects `window`, on the visible pages of `interval`, through each page's spatial index:
/// the cost is a few binary searches per page plus the primitives near the window, never the
/// rest of a (possibly very long) row. Primitives may be visited that lie just outside the
/// window; callers project and test each one.
pub(crate) fn for_each_row_window_primitive(
    scene: &AlignmentPresentationScene,
    interval: GenomicInterval,
    row: u32,
    window: Range<u64>,
    mut visit: impl FnMut(u64, AlignmentSemanticPrimitive),
) {
    let rows = row..row.saturating_add(1);
    let mut ranges = Vec::new();
    for page in scene.visible_pages(interval, rows.clone()) {
        ranges.clear();
        page.window_primitive_ranges(rows.clone(), window.clone(), &mut ranges);
        let page_start = page.interval().start();
        for range in &ranges {
            for primitive in &page.primitives()[range.start as usize..range.end as usize] {
                visit(page_start, *primitive);
            }
        }
    }
}

/// Smallest extent, in points, any projected alignment shape may have along either axis. One
/// point is at least one physical pixel at every window scale factor. Keep in lockstep with
/// `project_instance` in `rgv-render`'s `alignment_scene.wgsl`.
pub(crate) const MINIMUM_PRIMITIVE_EXTENT: f64 = 1.0;

/// Fixed part, in points, of the margin [`AlignmentCamera::projection_window`] adds beyond the
/// clip on each side. Twice the widest point marker; the read-height part covers arrowheads.
pub const PROJECTION_WINDOW_MARGIN_POINTS: f64 = 2.0 * POINT_MARKER_MAXIMUM_WIDTH;

/// Shortest drawn read, in points, on which any text is drawn: shorter labels and base letters
/// are refused by `reconcile_labels`, and insertion chips need more
/// ([`INSERTION_LABEL_MINIMUM_HEIGHT`]).
const LABEL_MINIMUM_TEXT_HEIGHT: f64 = 5.0;

/// Point markers (insertions, padding, specialist insertions) are I-beams centred on a base
/// boundary: a narrow vertical bar, a quarter of a base wide but within
/// [`INSERTION_BAR_MINIMUM_WIDTH`, `INSERTION_BAR_MAXIMUM_WIDTH`] points, with a horizontal
/// serif across the top and bottom of the read that reaches [`INSERTION_SERIF_WIDTH`] points
/// beyond the bar on each side. Every size is in points and capped, never a whole base, so the
/// mark cannot be mistaken for a base cell at any zoom. Keep in lockstep with shapes 2 and 5 of
/// `project_instance` and `fragment_main` in `rgv-render`'s `alignment_scene.wgsl` and with
/// `project_alignment_gpu_instance`.
pub const INSERTION_BAR_BASE_FRACTION: f64 = 0.25;
pub const INSERTION_BAR_MINIMUM_WIDTH: f64 = 2.0;
pub const INSERTION_BAR_MAXIMUM_WIDTH: f64 = 3.0;
/// How far each serif reaches beyond the bar, in points.
pub const INSERTION_SERIF_WIDTH: f64 = 2.5;
/// A serif's thickness is this fraction of the read height, within
/// [`INSERTION_SERIF_MINIMUM_THICKNESS`, `INSERTION_SERIF_MAXIMUM_THICKNESS`] points.
pub const INSERTION_SERIF_THICKNESS_FRACTION: f64 = 0.15;
pub const INSERTION_SERIF_MINIMUM_THICKNESS: f64 = 1.0;
pub const INSERTION_SERIF_MAXIMUM_THICKNESS: f64 = 2.0;
/// The widest an I-beam ever is: the widest bar plus both serifs.
pub(crate) const POINT_MARKER_MAXIMUM_WIDTH: f64 =
    INSERTION_BAR_MAXIMUM_WIDTH + 2.0 * INSERTION_SERIF_WIDTH;

/// Horizontal extent `(left, width)` of a point marker whose one-base span projects to
/// `cell_left..cell_right`, under a camera of `points_per_base`: centred on the cell's start
/// edge, or on its end edge when `at_end` (`AlignmentGeometryToken::PointMarkerAtEnd`). From
/// [`BOUNDARY_MARK_FULL_POINTS_PER_BASE`] it is the whole I-beam (its bar plus both serifs; see
/// [`insertion_mark_covers`] for the shape inside); below that a one-point hairline on that
/// boundary, and below [`BOUNDARY_MARK_HAIRLINE_POINTS_PER_BASE`] it is not drawn (`None`).
#[must_use]
pub fn point_marker_extent(
    cell_left: f64,
    cell_right: f64,
    at_end: bool,
    points_per_base: f64,
) -> Option<(f64, f64)> {
    let width = match boundary_mark_detail(points_per_base) {
        BoundaryMarkDetail::Full => {
            insertion_bar_width(cell_right - cell_left) + 2.0 * INSERTION_SERIF_WIDTH
        }
        BoundaryMarkDetail::Hairline => BOUNDARY_MARK_HAIRLINE_WIDTH,
        BoundaryMarkDetail::Hidden => return None,
    };
    let boundary = if at_end { cell_right } else { cell_left };
    Some((boundary - width * 0.5, width))
}

/// Width of an I-beam's vertical bar when one base is `cell` points wide.
#[must_use]
pub fn insertion_bar_width(cell: f64) -> f64 {
    (cell * INSERTION_BAR_BASE_FRACTION)
        .clamp(INSERTION_BAR_MINIMUM_WIDTH, INSERTION_BAR_MAXIMUM_WIDTH)
}

/// Thickness of an I-beam's serifs on a read drawn `height` points tall.
#[must_use]
pub fn insertion_serif_thickness(height: f64) -> f64 {
    (height * INSERTION_SERIF_THICKNESS_FRACTION).clamp(
        INSERTION_SERIF_MINIMUM_THICKNESS,
        INSERTION_SERIF_MAXIMUM_THICKNESS,
    )
}

/// Whether the point `(local_x, local_y)`, relative to the top-left of a full-size I-beam of
/// `width` x `height` points (see [`point_marker_extent`]), is painted: inside the centred bar,
/// or inside the top or bottom serif. Mirrors `fragment_main` in `alignment_scene.wgsl`.
#[must_use]
pub fn insertion_mark_covers(local_x: f64, local_y: f64, width: f64, height: f64) -> bool {
    let bar = (width - 2.0 * INSERTION_SERIF_WIDTH).max(0.0);
    let serif = insertion_serif_thickness(height);
    (local_x - width * 0.5).abs() < bar * 0.5 || local_y < serif || local_y > height - serif
}

/// The I-beam a full-size point marker draws, in unclipped pane coordinates, so a presenter or
/// exporter can draw its three parts even where the pane clips the primitive's box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuInsertionMark {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CpuInsertionMark {
    /// The bar, the top serif and the bottom serif, as `(x, y, width, height)` rectangles; see
    /// [`insertion_mark_covers`].
    #[must_use]
    pub fn parts(self) -> [(f64, f64, f64, f64); 3] {
        let bar = (self.width - 2.0 * INSERTION_SERIF_WIDTH).max(0.0);
        let serif = insertion_serif_thickness(self.height);
        [
            (self.x + (self.width - bar) * 0.5, self.y, bar, self.height),
            (self.x, self.y, self.width, serif),
            (self.x, self.y + self.height - serif, self.width, serif),
        ]
    }
}

/// Clip cut edges (shapes 6..=13 of `project_instance` and `fragment_main` in `rgv-render`'s
/// `alignment_scene.wgsl`, mirrored by `project_alignment_gpu_instance`; keep in lockstep). A
/// soft clip's sawtooth is half a base wide within [2, 6] points; a hard clip's bar is two.
const SOFT_CLIP_EDGE_BASE_FRACTION: f64 = 0.5;
const SOFT_CLIP_EDGE_MINIMUM_WIDTH: f64 = 2.0;
pub(crate) const SOFT_CLIP_EDGE_MAXIMUM_WIDTH: f64 = 6.0;
const HARD_CLIP_EDGE_WIDTH: f64 = 2.0;
/// Stroke of the sawtooth, measured across the stroke, and the target height of one tooth.
const CLIP_EDGE_STROKE: f64 = 1.5;
const CLIP_EDGE_TOOTH_HEIGHT: f64 = 4.0;
/// The direction arrowhead of `fragment_main` (shapes 3 and 4): its length along the read is
/// this fraction of the read height, but at most this many bases, and a head shorter than this
/// many points is not drawn; see [`arrowhead_length`].
pub const ARROWHEAD_LENGTH_FRACTION: f64 = 0.55;
pub const ARROWHEAD_MAXIMUM_BASES: f64 = 10.0;
pub const ARROWHEAD_MINIMUM_LENGTH: f64 = 0.5;

/// Horizontal extent `(left, width)` of a clip's cut edge whose one-base span (the aligned base
/// at the clipped boundary) projects to `cell_left..cell_right`, for a read drawn `height`
/// points tall. The edge lies just inside the read: its outer side is the boundary (the cell's
/// start edge, or its end edge when `edge.at_end`), so it never covers a ghost base beyond it.
///
/// When the read's direction arrowhead is on the same boundary (`edge.behind_arrow`) the edge
/// is moved inward from that boundary (the read's tip) by exactly the arrowhead's length,
/// [`arrowhead_length`] of `height` at `points_per_base`. The edge then starts where the
/// arrowhead's base is, so tip and cut both stay visible.
///
/// Below [`BOUNDARY_MARK_FULL_POINTS_PER_BASE`] either kind of edge is a one-point hairline in
/// the same place (drawn plain, see [`CpuPrimitiveShape::BoundaryHairline`]); below
/// [`BOUNDARY_MARK_HAIRLINE_POINTS_PER_BASE`] it is not drawn (`None`).
#[must_use]
pub fn clip_edge_extent(
    cell_left: f64,
    cell_right: f64,
    height: f64,
    edge: ClipEdgeToken,
    points_per_base: f64,
) -> Option<(f64, f64)> {
    let cell = cell_right - cell_left;
    let width = match boundary_mark_detail(points_per_base) {
        BoundaryMarkDetail::Full if edge.hard => HARD_CLIP_EDGE_WIDTH,
        BoundaryMarkDetail::Full => (cell * SOFT_CLIP_EDGE_BASE_FRACTION)
            .clamp(SOFT_CLIP_EDGE_MINIMUM_WIDTH, SOFT_CLIP_EDGE_MAXIMUM_WIDTH),
        BoundaryMarkDetail::Hairline => BOUNDARY_MARK_HAIRLINE_WIDTH,
        BoundaryMarkDetail::Hidden => return None,
    };
    if !edge.behind_arrow {
        return Some(if edge.at_end {
            (cell_right - width, width)
        } else {
            (cell_left, width)
        });
    }
    // Direction terminals keep their tip on the read's boundary (see `directional_extent`).
    let arrowhead = arrowhead_length(height, points_per_base);
    Some(if edge.at_end {
        (cell_right - arrowhead - width, width)
    } else {
        (cell_left + arrowhead, width)
    })
}

/// Whether the point `(local_x, local_y)`, relative to the top-left of a clip edge of `width` x
/// `height` points, is painted. A hard clip's bar is solid. A soft clip's edge is a zigzag
/// stroke whose vertices alternate between the read boundary (the outer side) and the inner
/// side, with teeth about four points tall. Mirrors `fragment_main` in `alignment_scene.wgsl`.
#[must_use]
pub fn clip_edge_covers(
    local_x: f64,
    local_y: f64,
    width: f64,
    height: f64,
    edge: ClipEdgeToken,
) -> bool {
    if edge.hard {
        return true;
    }
    let teeth = (height / CLIP_EDGE_TOOTH_HEIGHT + 0.5).floor().max(1.0);
    let period = height / teeth;
    let cycles = local_y / period;
    let phase = cycles - cycles.floor();
    let triangle = 1.0 - (2.0 * phase - 1.0).abs();
    let amplitude = (width - CLIP_EDGE_STROKE).max(0.0);
    let centre = CLIP_EDGE_STROKE * 0.5 + amplitude * triangle;
    let slope = 2.0 * amplitude / period;
    let half_stroke = CLIP_EDGE_STROKE * 0.5 * (1.0 + slope * slope).sqrt();
    let from_boundary = if edge.at_end {
        width - local_x
    } else {
        local_x
    };
    (from_boundary - centre).abs() <= half_stroke
}

pub(crate) fn project_primitive(
    primitive: AlignmentSemanticPrimitive,
    page_start: u64,
    global_row: u32,
    camera: AlignmentCamera,
    text: Option<Arc<str>>,
    interaction: AlignmentInteractionToken,
) -> Option<CpuProjectedPrimitive> {
    // Mirror of `project_instance` in `alignment_scene.wgsl`: project the unclipped genomic span,
    // apply the minimum-size policy, then clip to the pane exactly as the GPU scissor does. Every
    // shape keeps at least one point (>= one physical pixel) in both axes, so sub-pixel
    // mismatches do not shimmer and rows do not vanish when they are shorter than a pixel.
    let genomic_start = page_start.saturating_add(primitive.start_offset);
    let genomic_end = page_start.saturating_add(primitive.end_offset);
    if genomic_start >= genomic_end {
        return None;
    }
    let mut x = camera.x_for_position(genomic_start);
    let mut width = camera.x_for_position(genomic_end) - x;
    let row_top = camera.clip.y
        + (f64::from(global_row) - camera.row_scroll) * camera.row_height
        + (camera.row_height - camera.read_height) * 0.5;
    let body_height = camera.read_height.max(MINIMUM_PRIMITIVE_EXTENT);
    let points_per_base = 1.0 / camera.bases_per_pixel();
    let mut arrow = None;
    let mut insertion_mark = None;
    let (shape, y, height) = match primitive.geometry {
        AlignmentGeometryToken::EventLine => {
            width = width.max(MINIMUM_PRIMITIVE_EXTENT);
            (
                CpuPrimitiveShape::EventLine,
                row_top + camera.read_height * 0.45,
                (camera.read_height * 0.1).max(MINIMUM_PRIMITIVE_EXTENT),
            )
        }
        AlignmentGeometryToken::PointMarker | AlignmentGeometryToken::PointMarkerAtEnd => {
            let at_end = primitive.geometry == AlignmentGeometryToken::PointMarkerAtEnd;
            if matches!(primitive.component, AlignmentComponent::Label { .. }) {
                // A boundary-anchored label is a one-point box centred on the boundary its
                // event's I-beam is drawn on; `reconcile_labels` places the text beside the
                // I-beam. A one-base box would be clipped off-centre near the pane edge.
                let boundary = if at_end { x + width } else { x };
                width = MINIMUM_PRIMITIVE_EXTENT;
                x = boundary - width * 0.5;
                (CpuPrimitiveShape::TextLabel, row_top, body_height)
            } else {
                (x, width) = point_marker_extent(x, x + width, at_end, points_per_base)?;
                let shape = match boundary_mark_detail(points_per_base) {
                    BoundaryMarkDetail::Full => {
                        insertion_mark = Some(CpuInsertionMark {
                            x,
                            y: row_top,
                            width,
                            height: body_height,
                        });
                        CpuPrimitiveShape::PointMarker
                    }
                    _ => CpuPrimitiveShape::BoundaryHairline,
                };
                (shape, row_top, body_height)
            }
        }
        AlignmentGeometryToken::ClipEdge(edge) => {
            if matches!(primitive.component, AlignmentComponent::Label { .. }) {
                // A clip length keeps its anchor span; `reconcile_labels` places the text
                // outside the read, beyond that span.
                width = width.max(MINIMUM_PRIMITIVE_EXTENT);
                (CpuPrimitiveShape::TextLabel, row_top, body_height)
            } else {
                (x, width) = clip_edge_extent(x, x + width, body_height, edge, points_per_base)?;
                let shape = match boundary_mark_detail(points_per_base) {
                    BoundaryMarkDetail::Full => CpuPrimitiveShape::ClipEdge,
                    _ => CpuPrimitiveShape::BoundaryHairline,
                };
                (shape, row_top, body_height)
            }
        }
        AlignmentGeometryToken::ClipGhost => {
            if !clip_ghost_visible(points_per_base) {
                return None;
            }
            width = width.max(MINIMUM_PRIMITIVE_EXTENT);
            (CpuPrimitiveShape::ClipGhostCell, row_top, body_height)
        }
        AlignmentGeometryToken::DirectionalTerminalLeft => {
            (x, width) = directional_extent(x, x + width, false);
            // Every primitive cut by one read's arrow derives the tip from the same genomic
            // position, so they all carry the identical arrow.
            arrow = Some(CpuArrowhead {
                tip_x: camera.x_for_position(genomic_start),
                centre_y: row_top + body_height * 0.5,
                height: body_height,
                points_per_base,
                points_right: false,
            });
            (
                CpuPrimitiveShape::DirectionTerminalLeft,
                row_top,
                body_height,
            )
        }
        AlignmentGeometryToken::DirectionalTerminalRight => {
            (x, width) = directional_extent(x, x + width, true);
            arrow = Some(CpuArrowhead {
                tip_x: camera.x_for_position(genomic_end),
                centre_y: row_top + body_height * 0.5,
                height: body_height,
                points_per_base,
                points_right: true,
            });
            (
                CpuPrimitiveShape::DirectionTerminalRight,
                row_top,
                body_height,
            )
        }
        AlignmentGeometryToken::InsideArrowhead(token) => {
            // A cell (or block piece) short of the tip: its own shape, cut by the read's arrow
            // whose tip is `bases_from_tip` bases beyond its tip-side edge. Like the terminal
            // shapes its one-point minimum grows away from the tip, so that edge stays put.
            (x, width) = directional_extent(x, x + width, token.points_right());
            let bases = u64::from(token.bases_from_tip());
            arrow = Some(CpuArrowhead {
                tip_x: camera.x_for_position(if token.points_right() {
                    genomic_end.saturating_add(bases)
                } else {
                    genomic_start.saturating_sub(bases)
                }),
                centre_y: row_top + body_height * 0.5,
                height: body_height,
                points_per_base,
                points_right: token.points_right(),
            });
            (rectangle_shape(primitive.component), row_top, body_height)
        }
        AlignmentGeometryToken::Rectangle => {
            width = width.max(MINIMUM_PRIMITIVE_EXTENT);
            (rectangle_shape(primitive.component), row_top, body_height)
        }
    };
    let clip_right = camera.clip.x + camera.clip.width;
    let clip_bottom = camera.clip.y + camera.clip.height;
    let right = (x + width).min(clip_right);
    let bottom = (y + height).min(clip_bottom);
    x = x.max(camera.clip.x);
    let y = y.max(camera.clip.y);
    width = right - x;
    let height = bottom - y;
    (width > 0.0 && height > 0.0).then_some(CpuProjectedPrimitive {
        x,
        y,
        width,
        height,
        shape,
        geometry: primitive.geometry,
        handle: primitive.handle,
        component: primitive.component,
        style: primitive.style,
        interaction,
        draw_order: primitive.draw_order,
        text,
        row: global_row,
        arrow,
        insertion_mark,
    })
}

/// The shape of a plain rectangle (or one cut by its read's arrow, see
/// [`AlignmentGeometryToken::InsideArrowhead`]) of `component`.
const fn rectangle_shape(component: AlignmentComponent) -> CpuPrimitiveShape {
    match component {
        AlignmentComponent::Label { .. } => CpuPrimitiveShape::TextLabel,
        AlignmentComponent::Base { .. }
        | AlignmentComponent::ModifiedBase { .. }
        | AlignmentComponent::BisulfiteBase { .. }
        | AlignmentComponent::SmrtKinetics { .. }
        | AlignmentComponent::SbxTailBase { .. } => CpuPrimitiveShape::BaseCell,
        _ => CpuPrimitiveShape::ReadBody,
    }
}

fn merge_label_fragments(existing: &mut CpuProjectedPrimitive, fragment: &CpuProjectedPrimitive) {
    debug_assert_eq!(existing.handle, fragment.handle);
    debug_assert_eq!(existing.component, fragment.component);
    debug_assert_eq!(existing.row, fragment.row);
    debug_assert_eq!(existing.text, fragment.text);
    let right = (existing.x + existing.width).max(fragment.x + fragment.width);
    existing.x = existing.x.min(fragment.x);
    existing.width = right - existing.x;
    existing.y = existing.y.min(fragment.y);
    existing.height = existing.height.max(fragment.height);
    existing.draw_order = existing.draw_order.min(fragment.draw_order);
}

/// CIGAR event lengths (insertions, deletions, skips, clips) are drawn only where the text has
/// room, so coarse views are not covered in numbers:
///
/// * a span event (deletion, reference skip) shows its length only when the text fits inside
///   the event's on-screen extent;
/// * a clip has no on-screen extent, so its length shows only from this many points per base,
///   and only when the text fits in the width its `length` unseen bases would occupy at the
///   current zoom (`length x points per base`);
/// * an insertion at least `minimum_event_label_size` long has a length chip beside its
///   I-beam; see `place_insertion_label` for when and where it is shown.
pub const EVENT_LABEL_MINIMUM_POINTS_PER_BASE: f64 = 2.0;

/// Insertion label anchors of at most this many bases carry the inserted bases themselves
/// (`+T`, `+AG`), longer ones their length (`+12`). A chip always shows the length (see
/// `place_insertion_label`).
pub const INSERTION_LABEL_MAXIMUM_BASES: u32 = 4;
/// Reads drawn shorter than this many points carry no insertion label: its chip could not hold
/// the smallest text.
pub const INSERTION_LABEL_MINIMUM_HEIGHT: f64 = 8.0;
/// Gap, in points, between an I-beam's serifs and its label.
const INSERTION_LABEL_GAP: f64 = 1.0;
/// Points per base from which base letters (and so insertion bases) are drawn.
const MINIMUM_BASE_GLYPH_WIDTH: f64 = CLIP_GHOST_MINIMUM_POINTS_PER_BASE;
const LABEL_HORIZONTAL_PADDING: f64 = 2.0;
const LABEL_GLYPH_ASPECT_RATIO: f64 = 0.56;

/// The label of an insertion of `length` bases: `+` and the inserted read bases (upper case)
/// when there are at most [`INSERTION_LABEL_MAXIMUM_BASES`] and the record carries them as
/// letters, otherwise `+` and the length. `bases` is the inserted slice of the read's `SEQ`.
#[must_use]
pub fn insertion_label_text(bases: Option<&[u8]>, length: u32) -> String {
    match bases {
        Some(bases)
            if length <= INSERTION_LABEL_MAXIMUM_BASES
                && !bases.is_empty()
                && u32::try_from(bases.len()) == Ok(length)
                && bases.iter().all(u8::is_ascii_alphabetic) =>
        {
            std::iter::once('+')
                .chain(
                    bases
                        .iter()
                        .map(|base| char::from(base.to_ascii_uppercase())),
                )
                .collect()
        }
        _ => format!("+{length}"),
    }
}

/// The length-only form of an insertion label (`+AG` becomes `+2`); `None` when it already is.
fn insertion_length_only(text: &str) -> Option<String> {
    let bases = text.strip_prefix('+')?;
    (!bases.is_empty() && !bases.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| format!("+{}", bases.chars().count()))
}

/// Estimated width of `text` laid out on a read `height` points tall, padding included.
fn label_text_width(text: &str, height: f64) -> f64 {
    text.chars().count().max(1) as f64 * height * LABEL_GLYPH_ASPECT_RATIO
        + LABEL_HORIZONTAL_PADDING * 2.0
}

/// Places an insertion's label (a label whose one-point box is centred on the insertion's
/// boundary) as a chip the read's height tall, just beside the I-beam: after it, or before it
/// when that side has no room. Only an insertion at least `minimum_event_label_size` long
/// ([`AlignmentLabelKind::InsertionLength`]) has a chip, and only from
/// [`EVENT_LABEL_MINIMUM_POINTS_PER_BASE`] on reads at least [`INSERTION_LABEL_MINIMUM_HEIGHT`]
/// points tall; it always states the length (`+14`, never the bases). A shorter insertion
/// ([`AlignmentLabelKind::ShortInsertion`]) is its I-beam alone at every zoom; hovering it
/// tells the rest.
///
/// A side has room only when the chip stays in the pane and covers nothing but the plain body
/// of its own read: no other read, and no event of any read (substitution and modification
/// cells, other I-beams, deletion gaps and their edges, clip edges and ghosts; see
/// [`RowOccupancy::blocks_chip`]). With neither side free the chip is dropped, never moved
/// further, and nothing else is hidden for it. Collisions with other labels are resolved by the
/// caller's sweep.
fn place_insertion_label(
    mut label: CpuProjectedPrimitive,
    kind: AlignmentLabelKind,
    points_per_base: f64,
    clip: CpuClipRect,
    occupancy: &RowOccupancy,
) -> Option<CpuProjectedPrimitive> {
    if kind != AlignmentLabelKind::InsertionLength
        || points_per_base < EVENT_LABEL_MINIMUM_POINTS_PER_BASE
        || label.height < INSERTION_LABEL_MINIMUM_HEIGHT
    {
        return None;
    }
    if let Some(length) = label.text.as_deref().and_then(insertion_length_only) {
        label.text = Some(Arc::from(length));
    }
    let text_width = label_text_width(label.text.as_deref()?, label.height);
    let boundary = label.x + label.width * 0.5;
    let reach = point_marker_extent(boundary, boundary + points_per_base, false, points_per_base)
        .map_or(0.0, |(_, width)| width * 0.5);
    let clip_right = clip.x + clip.width;
    let has_room = |left: f64| {
        let right = left + text_width;
        left >= clip.x
            && right <= clip_right
            && !occupancy.blocks_chip(label.row, label.handle, left, right)
    };
    let after = boundary + reach + INSERTION_LABEL_GAP;
    let before = boundary - reach - INSERTION_LABEL_GAP - text_width;
    label.x = [after, before].into_iter().find(|&left| has_room(left))?;
    label.width = text_width;
    Some(label)
}

/// Whether a base cell or clip-ghost base anchor certainly projects narrower than a base letter
/// ([`MINIMUM_BASE_GLYPH_WIDTH`]) under a camera of `points_per_base`, so `reconcile_labels`
/// would refuse its letter. Only the shapes whose projected width is their span (at least one
/// point) are decided here, with a margin for rounding; any other anchor is projected and
/// decided by `reconcile_labels` as before.
fn base_glyph_cannot_fit(primitive: AlignmentSemanticPrimitive, points_per_base: f64) -> bool {
    if !matches!(
        primitive.component,
        AlignmentComponent::Base { .. } | AlignmentComponent::ClipGhostBase { .. }
    ) || !matches!(
        primitive.geometry,
        AlignmentGeometryToken::Rectangle
            | AlignmentGeometryToken::ClipGhost
            | AlignmentGeometryToken::InsideArrowhead(_)
            | AlignmentGeometryToken::DirectionalTerminalLeft
            | AlignmentGeometryToken::DirectionalTerminalRight
    ) {
        return false;
    }
    let span = primitive.end_offset.saturating_sub(primitive.start_offset) as f64;
    (span * points_per_base).max(MINIMUM_PRIMITIVE_EXTENT) < MINIMUM_BASE_GLYPH_WIDTH - 1.0e-6
}

/// Whether `label` is an insertion's label chip; see [`place_insertion_label`].
#[must_use]
pub const fn is_insertion_label(component: AlignmentComponent) -> bool {
    matches!(
        component,
        AlignmentComponent::Label {
            kind: AlignmentLabelKind::InsertionLength | AlignmentLabelKind::ShortInsertion,
            ..
        }
    )
}

/// The event length a point-event label states: its leading decimal digits (a hard clip's
/// length carries an `H` suffix). The page builder writes these texts, so they always parse.
fn point_event_label_length(text: &str) -> f64 {
    let digits = text.trim_end_matches(|character: char| !character.is_ascii_digit());
    digits.parse::<u32>().map_or(1.0, f64::from).max(1.0)
}

/// What occupies each row of the pane, for labels placed beside a read. A clip length or an
/// insertion chip must not sit on another read's body packed into the same row; an insertion
/// chip must also cover no event on any read (see [`is_chip_obstacle`]).
#[derive(Debug, Default)]
struct RowOccupancy {
    rows: HashMap<u32, Vec<RowOccupant>>,
}

#[derive(Clone, Copy, Debug)]
struct RowOccupant {
    left: f64,
    right: f64,
    handle: AlignmentHandle,
    body: bool,
    event: bool,
}

/// Slack, in points, below which two extents that merely touch are not said to overlap.
const OCCUPANCY_TOLERANCE: f64 = 1.0e-6;

impl RowOccupancy {
    fn add(&mut self, primitive: &CpuProjectedPrimitive) {
        let body = is_read_body(primitive.component);
        let event = is_chip_obstacle(primitive.component);
        if body || event {
            self.rows
                .entry(primitive.row)
                .or_default()
                .push(RowOccupant {
                    left: primitive.x,
                    right: primitive.x + primitive.width,
                    handle: primitive.handle,
                    body,
                    event,
                });
        }
    }

    /// Whether `left..right` of `row` lies on the body of a read other than `handle`.
    fn covers_another_read(
        &self,
        row: u32,
        handle: AlignmentHandle,
        left: f64,
        right: f64,
    ) -> bool {
        self.rows.get(&row).is_some_and(|occupants| {
            occupants.iter().any(|occupant| {
                occupant.body
                    && occupant.handle != handle
                    && occupant.left < right - OCCUPANCY_TOLERANCE
                    && occupant.right > left + OCCUPANCY_TOLERANCE
            })
        })
    }

    /// Whether an insertion chip of `handle` over `left..right` of `row` would hide anything:
    /// another read's body, or an event of any read, including the edges of its extent (each
    /// event is kept [`INSERTION_LABEL_GAP`] clear).
    fn blocks_chip(&self, row: u32, handle: AlignmentHandle, left: f64, right: f64) -> bool {
        self.covers_another_read(row, handle, left, right)
            || self.rows.get(&row).is_some_and(|occupants| {
                occupants.iter().any(|occupant| {
                    occupant.event
                        && occupant.left - INSERTION_LABEL_GAP < right - OCCUPANCY_TOLERANCE
                        && occupant.right + INSERTION_LABEL_GAP > left + OCCUPANCY_TOLERANCE
                })
            })
    }
}

fn is_read_body(component: AlignmentComponent) -> bool {
    matches!(
        component,
        AlignmentComponent::ReadSpan
            | AlignmentComponent::AlignedSegment { .. }
            | AlignmentComponent::DirectionTerminal
            | AlignmentComponent::Deletion { .. }
            | AlignmentComponent::ReferenceSkip { .. }
    )
}

/// Everything drawn on (or beside) a read that an insertion chip must not cover: base and
/// substitution cells, modification and other specialist cells, I-beams and hairlines, deletion
/// and skip gaps, clip edges and ghost bases. Only the plain read body (and pair connectors,
/// drawn between mates) may lie under a chip.
fn is_chip_obstacle(component: AlignmentComponent) -> bool {
    !matches!(
        component,
        AlignmentComponent::ReadSpan
            | AlignmentComponent::AlignedSegment { .. }
            | AlignmentComponent::DirectionTerminal
            | AlignmentComponent::PairConnector { .. }
            | AlignmentComponent::Label { .. }
    )
}

/// The pages whose primitives can occupy a row beside a label: read geometry and specialist
/// overlays (modification cells and the like), never label pages.
fn occupancy_pages(
    scene: &AlignmentPresentationScene,
    covered_interval: Option<GenomicInterval>,
    covered_rows: Range<u32>,
) -> Vec<&Arc<crate::AlignmentSemanticPage>> {
    covered_interval.map_or_else(Vec::new, |interval| {
        scene
            .visible_pages(interval, covered_rows)
            .into_iter()
            .filter(|page| {
                matches!(
                    page.key().family(),
                    AlignmentPageFamily::ReadGeometry | AlignmentPageFamily::SpecialistOverlay
                )
            })
            .collect()
    })
}

fn reconcile_labels(
    primitives: &mut Vec<CpuProjectedPrimitive>,
    labels: impl Iterator<Item = CpuProjectedPrimitive>,
    camera: AlignmentCamera,
    occupancy: &RowOccupancy,
) {
    const MIN_TEXT_HEIGHT: f64 = LABEL_MINIMUM_TEXT_HEIGHT;
    const COLLISION_GAP: f64 = 2.0;

    let clip = camera.clip;
    let points_per_base = 1.0 / camera.bases_per_pixel();
    let clip_right = clip.x + clip.width;
    let mut base_labels = Vec::new();
    let mut candidates = labels
        .filter_map(|mut label| {
            if let AlignmentComponent::Label { kind, .. } = label.component
                && is_insertion_label(label.component)
            {
                return place_insertion_label(label, kind, points_per_base, clip, occupancy);
            }
            let text = label.text.as_deref()?;
            if matches!(
                label.component,
                AlignmentComponent::Base { .. } | AlignmentComponent::ClipGhostBase { .. }
            ) {
                // Match the accepted reference/read presentation boundary: seven horizontal
                // points make one nucleotide distinguishable, while the normal five-point read
                // body is already tall enough for its compact glyph. Requiring an eight-point
                // square made base letters impossible at the default read density.
                if label.width >= MINIMUM_BASE_GLYPH_WIDTH && label.height >= MIN_TEXT_HEIGHT {
                    base_labels.push(label);
                }
                return None;
            }
            if label.height < MIN_TEXT_HEIGHT {
                return None;
            }
            let text_width = label_text_width(text, label.height);
            if text_width > clip.width {
                return None;
            }
            let fits = match label.component {
                // Read labels and span-event lengths must fit inside their on-screen extent.
                AlignmentComponent::Label {
                    kind:
                        AlignmentLabelKind::ReadName
                        | AlignmentLabelKind::AuxiliaryTag(_)
                        | AlignmentLabelKind::DeletionLength
                        | AlignmentLabelKind::SkipLength,
                    ..
                } => text_width <= label.width,
                AlignmentComponent::Label {
                    kind: AlignmentLabelKind::ClipLength,
                    ..
                } => {
                    points_per_base >= EVENT_LABEL_MINIMUM_POINTS_PER_BASE
                        && text_width <= point_event_label_length(text) * points_per_base
                }
                _ => true,
            };
            if !fits {
                return None;
            }
            if let AlignmentGeometryToken::ClipEdge(edge) = label.geometry {
                // A clip length sits outside its read, never over a read body. Its anchor span
                // is the clipped boundary base plus the ghost bases built beyond it. When those
                // ghosts are drawn (see `clip_ghost_visible`) the text goes past them; when they
                // are hidden at this zoom it goes right at the read's clipped end, rather than
                // past invisible ghosts and onto the next read in the row. Off the pane, or on
                // any other read of the same row, the label is dropped rather than moved.
                let ghosts_drawn = clip_ghost_visible(points_per_base);
                let left = match (edge.at_end, ghosts_drawn) {
                    (true, true) => label.x + label.width,
                    (true, false) => label.x + points_per_base,
                    (false, true) => label.x - text_width,
                    (false, false) => label.x + label.width - points_per_base - text_width,
                };
                if left < clip.x || left + text_width > clip_right {
                    return None;
                }
                if occupancy.covers_another_read(label.row, label.handle, left, left + text_width) {
                    return None;
                }
                label.x = left;
            } else {
                let anchor = label.x + label.width * 0.5;
                label.x = (anchor - text_width * 0.5).clamp(clip.x, clip_right - text_width);
            }
            label.width = text_width;
            Some(label)
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.row.cmp(&right.row))
            .then_with(|| left.y.total_cmp(&right.y))
            .then_with(|| left.handle.cmp(&right.handle))
            .then_with(|| left.component.cmp(&right.component))
    });

    // A sweep line keeps only accepted labels whose horizontal bounds can still collide.
    // The active set is therefore bounded by current screen geometry, not a record-count cap.
    // Base letters are not part of the sweep: an insertion chip is only ever placed where it
    // covers no cell (see `place_insertion_label`), so no letter is hidden for one.
    let mut active = Vec::<CpuProjectedPrimitive>::new();
    for candidate in candidates {
        active.retain(|accepted| accepted.x + accepted.width + COLLISION_GAP > candidate.x);
        let collides = active.iter().any(|accepted| {
            accepted.y < candidate.y + candidate.height + COLLISION_GAP
                && candidate.y < accepted.y + accepted.height + COLLISION_GAP
        });
        if !collides {
            active.push(candidate.clone());
            primitives.push(candidate);
        }
    }
    primitives.append(&mut base_labels);
}

fn cpu_projection_scratch_bytes(primitives: usize, labels: usize) -> usize {
    let primitive_bytes = primitives.saturating_mul(size_of::<CpuProjectedPrimitive>());
    let label_vector_bytes = labels
        .saturating_mul(size_of::<CpuProjectedPrimitive>())
        .saturating_mul(3);
    let hash_entries = labels
        .saturating_mul(2)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX);
    let hash_bytes = hash_entries.saturating_mul(
        size_of::<(AlignmentHandle, AlignmentComponent)>()
            .saturating_add(size_of::<CpuProjectedPrimitive>())
            .saturating_add(size_of::<usize>()),
    );
    primitive_bytes
        .saturating_add(label_vector_bytes)
        .saturating_add(hash_bytes)
}

fn cpu_label_projection_scratch_bytes(labels: usize) -> usize {
    let label_vectors = labels
        .saturating_mul(size_of::<CpuProjectedPrimitive>())
        .saturating_mul(4);
    let hash_entries = labels
        .saturating_mul(2)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX);
    let hash_bytes = hash_entries.saturating_mul(
        size_of::<(AlignmentHandle, AlignmentComponent)>()
            .saturating_add(size_of::<CpuProjectedPrimitive>())
            .saturating_add(size_of::<usize>()),
    );
    label_vectors.saturating_add(hash_bytes)
}

fn intersect_intervals(left: GenomicInterval, right: GenomicInterval) -> Option<GenomicInterval> {
    (left.contig() == right.contig())
        .then(|| {
            GenomicInterval::new(
                left.contig(),
                left.start().max(right.start()),
                left.end().min(right.end()),
            )
            .ok()
        })
        .flatten()
}

const fn intersect_ranges(left: Range<u32>, right: Range<u32>) -> Range<u32> {
    let start = if left.start > right.start {
        left.start
    } else {
        right.start
    };
    let end = if left.end < right.end {
        left.end
    } else {
        right.end
    };
    if start < end {
        start..end
    } else {
        start..start
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CpuProjectionError {
    #[error("alignment CPU camera geometry is invalid")]
    InvalidCamera,
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CpuInteractionError {
    #[error("CPU alignment frame revision {frame:?} does not match scene revision {scene:?}")]
    StaleSceneFrame {
        frame: SceneRevision,
        scene: SceneRevision,
    },
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;
    use rgv_model::AlignmentSourcePosition;

    use super::*;
    use crate::{
        AlignmentBodyColorToken, AlignmentSourceRevision, RevisionFingerprint, SourceFingerprint,
    };

    const GEOMETRIES: [AlignmentGeometryToken; 11] = [
        AlignmentGeometryToken::Rectangle,
        AlignmentGeometryToken::EventLine,
        AlignmentGeometryToken::PointMarker,
        AlignmentGeometryToken::PointMarkerAtEnd,
        AlignmentGeometryToken::DirectionalTerminalLeft,
        AlignmentGeometryToken::DirectionalTerminalRight,
        AlignmentGeometryToken::ClipEdge(ClipEdgeToken {
            at_end: false,
            hard: false,
            behind_arrow: true,
        }),
        AlignmentGeometryToken::ClipEdge(ClipEdgeToken {
            at_end: true,
            hard: true,
            behind_arrow: false,
        }),
        AlignmentGeometryToken::ClipGhost,
        AlignmentGeometryToken::InsideArrowhead(crate::ArrowheadCellToken::new(true, 9).unwrap()),
        AlignmentGeometryToken::InsideArrowhead(crate::ArrowheadCellToken::new(false, 1).unwrap()),
    ];

    fn primitive(
        start_offset: u64,
        end_offset: u64,
        geometry: AlignmentGeometryToken,
    ) -> AlignmentSemanticPrimitive {
        AlignmentSemanticPrimitive {
            start_offset,
            end_offset,
            row_offset: 0,
            handle: AlignmentHandle::new(
                AlignmentSourceRevision::new(
                    SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                    RevisionFingerprint::new(3, 4),
                ),
                AlignmentSourcePosition::bam_virtual_offset(100),
            ),
            component: AlignmentComponent::ReadSpan,
            geometry,
            style: AlignmentStyleToken {
                flags: 0,
                mapping_quality: None,
                operation: None,
                body: AlignmentBodyColorToken::Neutral,
            },
            draw_order: 0,
        }
    }

    fn camera(bases: u64, row_height: f64, read_height: f64) -> AlignmentCamera {
        AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 1_000, 1_000 + bases).unwrap(),
            0.0,
            row_height,
            read_height,
            CpuClipRect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            },
        )
        .unwrap()
    }

    fn project(
        primitive: AlignmentSemanticPrimitive,
        row: u32,
        camera: AlignmentCamera,
    ) -> Option<CpuProjectedPrimitive> {
        project_primitive(
            primitive,
            1_000,
            row,
            camera,
            None,
            AlignmentInteractionToken::default(),
        )
    }

    #[test]
    fn cpu_projection_gives_every_shape_at_least_one_point_in_both_axes() {
        // 1,000 bases per point and rows a quarter of a point tall: a single mismatch is a
        // thousandth of a point wide and every read is shorter than a pixel.
        let camera = camera(100_000, 0.25, 0.2);
        for geometry in GEOMETRIES {
            let projected = project(primitive(50_000, 50_001, geometry), 8, camera);
            if matches!(
                geometry,
                AlignmentGeometryToken::PointMarker
                    | AlignmentGeometryToken::PointMarkerAtEnd
                    | AlignmentGeometryToken::ClipEdge(_)
                    | AlignmentGeometryToken::ClipGhost
            ) {
                // Exempt: boundary marks are not drawn below 0.1 points per base, and clip ghost
                // cells not below the base-letter width (they appear only with their letters).
                assert!(
                    projected.is_none(),
                    "{geometry:?} drawn at 0.001 points per base"
                );
                continue;
            }
            let projected =
                projected.unwrap_or_else(|| panic!("{geometry:?} vanished below one point"));
            assert!(
                projected.width >= 1.0 - 1.0e-9,
                "{geometry:?} is {} points wide",
                projected.width
            );
            assert!(
                projected.height >= 1.0 - 1.0e-9,
                "{geometry:?} is {} points tall",
                projected.height
            );
        }
    }

    /// Marks drawn on a read scale with the zoom by camera-global bands: the arrowhead is
    /// `min(0.55 x height, 10 x points per base)` long and absent below half a point; point
    /// markers and clip edges are full size from one point per base, one-point plain hairlines
    /// on the same boundary from 0.1, and neither drawn nor picked below that.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn read_marks_scale_with_the_zoom_in_bands() {
        let soft_start = ClipEdgeToken {
            at_end: false,
            hard: false,
            behind_arrow: false,
        };
        let soft_behind_end = ClipEdgeToken {
            at_end: true,
            hard: false,
            behind_arrow: true,
        };
        let hard_behind_start = ClipEdgeToken {
            at_end: false,
            hard: true,
            behind_arrow: true,
        };
        // (points per base, head, marker width, soft clip width, hard clip width, shape); a
        // width of zero means the mark is not drawn. Reads are 11 points tall.
        let bands = [
            (0.0625, 0.625, 0.0, 0.0, 0.0, None),
            // Under half a point the read ends square.
            (0.04, 0.0, 0.0, 0.0, 0.0, None),
            (
                0.4,
                4.0,
                1.0,
                1.0,
                1.0,
                Some(CpuPrimitiveShape::BoundaryHairline),
            ),
            (
                0.1,
                1.0,
                1.0,
                1.0,
                1.0,
                Some(CpuPrimitiveShape::BoundaryHairline),
            ),
            // Full size: an I-beam whose bar is a quarter base within [2, 3] points, plus two
            // 2.5-point serifs.
            (
                2.0,
                6.05,
                7.0,
                2.0,
                2.0,
                Some(CpuPrimitiveShape::PointMarker),
            ),
            (
                40.0,
                6.05,
                8.0,
                6.0,
                2.0,
                Some(CpuPrimitiveShape::PointMarker),
            ),
        ];
        for (points_per_base, head, marker, soft, hard, shape) in bands {
            let context = format!("{points_per_base} points per base");
            assert!(
                (arrowhead_length(11.0, points_per_base) - head).abs() < 1.0e-9,
                "{context}: head {}",
                arrowhead_length(11.0, points_per_base)
            );
            // Exactly 1,000 points wide, so the scale is exact.
            let bases = (1_000.0 / points_per_base).round() as u64;
            let camera = AlignmentCamera::new(
                GenomicInterval::new(ContigId::new(0), 1_000, 1_000 + bases).unwrap(),
                0.0,
                14.0,
                11.0,
                CpuClipRect {
                    x: 10.0,
                    y: 20.0,
                    width: 1_000.0,
                    height: 50.0,
                },
            )
            .unwrap();
            let base = bases / 2;
            let start_edge = 10.0 + base as f64 * points_per_base;
            let end_edge = start_edge + points_per_base;

            // The arrowhead every tip primitive is cut by.
            let terminal = project(
                primitive(
                    base,
                    base + 1,
                    AlignmentGeometryToken::DirectionalTerminalRight,
                ),
                0,
                camera,
            )
            .unwrap();
            let arrow = terminal.arrow.unwrap();
            assert!((arrow.length() - head).abs() < 1.0e-9, "{context}");
            let centre = arrow.centre_y;
            // Just inside the tip, off the centre line: cut away unless the head is gone.
            assert_eq!(
                arrow.covers(arrow.tip_x - 0.01, centre + 5.0),
                head == 0.0,
                "{context}"
            );

            for (geometry, boundary) in [
                (AlignmentGeometryToken::PointMarker, start_edge),
                (AlignmentGeometryToken::PointMarkerAtEnd, end_edge),
            ] {
                let projected = project(primitive(base, base + 1, geometry), 0, camera);
                assert_eq!(
                    point_marker_extent(start_edge, end_edge, false, points_per_base)
                        .map(|(_, width)| width),
                    (marker > 0.0).then_some(marker),
                    "{context}"
                );
                if marker == 0.0 {
                    assert!(projected.is_none(), "{context}: {projected:?}");
                    continue;
                }
                let projected = projected.unwrap();
                assert_eq!(Some(projected.shape), shape, "{context}");
                assert!((projected.width - marker).abs() < 1.0e-9, "{context}");
                assert!(
                    (projected.x + marker * 0.5 - boundary).abs() < 1.0e-9,
                    "{context}: {projected:?}"
                );
            }
            for edge in [soft_start, soft_behind_end, hard_behind_start] {
                let width = if edge.hard { hard } else { soft };
                let projected = project(
                    primitive(base, base + 1, AlignmentGeometryToken::ClipEdge(edge)),
                    0,
                    camera,
                );
                if width == 0.0 {
                    assert!(projected.is_none(), "{context}: {projected:?}");
                    continue;
                }
                let projected = projected.unwrap();
                let expected_shape = match shape {
                    Some(CpuPrimitiveShape::PointMarker) => CpuPrimitiveShape::ClipEdge,
                    _ => CpuPrimitiveShape::BoundaryHairline,
                };
                assert_eq!(projected.shape, expected_shape, "{context}");
                assert!((projected.width - width).abs() < 1.0e-9, "{context}");
                // On the boundary, moved behind the (new) head where the arrow shares it.
                let inset = if edge.behind_arrow { head } else { 0.0 };
                let expected_left = if edge.at_end {
                    end_edge - inset - width
                } else {
                    start_edge + inset
                };
                assert!(
                    (projected.x - expected_left).abs() < 1.0e-9,
                    "{context} {edge:?}: {projected:?}"
                );
            }
        }
    }

    #[test]
    fn point_markers_are_i_beams_on_a_base_boundary_and_labels_follow_them() {
        // 112 points per base.
        let camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 1_000, 1_010).unwrap(),
            0.0,
            14.0,
            11.0,
            CpuClipRect {
                x: 10.0,
                y: 20.0,
                width: 1_120.0,
                height: 50.0,
            },
        )
        .unwrap();
        let start_edge = 10.0 + 5.0 * 112.0;
        let end_edge = start_edge + 112.0;
        for (geometry, boundary) in [
            (AlignmentGeometryToken::PointMarker, start_edge),
            (AlignmentGeometryToken::PointMarkerAtEnd, end_edge),
        ] {
            // A three-point bar plus two 2.5-point serifs, centred on the boundary.
            let marker = project(primitive(5, 6, geometry), 0, camera).unwrap();
            assert_eq!(marker.shape, CpuPrimitiveShape::PointMarker);
            assert!((marker.width - 8.0).abs() < 1.0e-9, "{marker:?}");
            assert!((marker.x + 4.0 - boundary).abs() < 1.0e-9, "{marker:?}");
            assert!((marker.height - 11.0).abs() < 1.0e-9);
            assert_eq!(
                marker.insertion_mark,
                Some(CpuInsertionMark {
                    x: marker.x,
                    y: marker.y,
                    width: 8.0,
                    height: 11.0,
                })
            );

            // A label carrying the same token is a one-point box centred on the boundary
            // (changed from a one-base box, which the pane could clip off-centre).
            let mut label = primitive(5, 6, geometry);
            label.component = AlignmentComponent::Label {
                kind: AlignmentLabelKind::InsertionLength,
                id: crate::ContractFingerprint::new(1),
            };
            let label = project(label, 0, camera).unwrap();
            assert_eq!(label.shape, CpuPrimitiveShape::TextLabel);
            assert!((label.width - 1.0).abs() < 1.0e-9);
            assert!((label.x + label.width * 0.5 - boundary).abs() < 1.0e-9);
        }
        // A span-anchored label is unchanged: centred on its base cell.
        let mut label = primitive(5, 6, AlignmentGeometryToken::Rectangle);
        label.component = AlignmentComponent::Label {
            kind: AlignmentLabelKind::ClipLength,
            id: crate::ContractFingerprint::new(1),
        };
        let label = project(label, 0, camera).unwrap();
        assert!((label.x - start_edge).abs() < 1.0e-9);
        assert!((label.width - 112.0).abs() < 1.0e-9);
    }

    #[test]
    fn insertion_labels_state_short_insertions_by_their_bases_and_long_ones_by_length() {
        for (bases, length, text) in [
            (Some(&b"t"[..]), 1, "+T"),
            (Some(&b"AG"[..]), 2, "+AG"),
            (Some(&b"acgt"[..]), 4, "+ACGT"),
            // Longer than INSERTION_LABEL_MAXIMUM_BASES: the length.
            (Some(&b"ACGTA"[..]), 5, "+5"),
            (None, 12, "+12"),
            // Missing or unusable bases fall back to the length.
            (Some(&b"*"[..]), 1, "+1"),
            (Some(&b"AC"[..]), 3, "+3"),
        ] {
            assert_eq!(insertion_label_text(bases, length), text);
        }
        assert_eq!(INSERTION_LABEL_MAXIMUM_BASES, 4);
        assert_eq!(insertion_length_only("+AG").as_deref(), Some("+2"));
        assert_eq!(insertion_length_only("+12"), None);
    }

    /// A projected primitive of `component` over `left..right` of row 0.
    fn occupant(
        component: AlignmentComponent,
        left: f64,
        right: f64,
        position: u64,
    ) -> CpuProjectedPrimitive {
        let mut semantic = primitive(0, 1, AlignmentGeometryToken::Rectangle);
        semantic.component = component;
        semantic.handle = AlignmentHandle::new(semantic.handle.source_revision(), {
            AlignmentSourcePosition::bam_virtual_offset(position)
        });
        CpuProjectedPrimitive {
            x: left,
            y: 20.0,
            width: right - left,
            height: 11.0,
            shape: CpuPrimitiveShape::BaseCell,
            geometry: AlignmentGeometryToken::Rectangle,
            handle: semantic.handle,
            component,
            style: semantic.style,
            interaction: AlignmentInteractionToken::default(),
            draw_order: 0,
            text: None,
            row: 0,
            arrow: None,
            insertion_mark: None,
        }
    }

    /// The chip rules at the level of one label: only an insertion at least the minimum event
    /// label size has a chip, it states its length, and each side is refused by any event of any
    /// kind under it or within a point of it (modification cells and clip ghosts included) and by
    /// another read's body, but not by its own read's plain body.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn insertion_chips_avoid_every_kind_of_event() {
        // 20 points per base: the I-beam reaches four points either side of its boundary at
        // x = 100, and `+14` is 22.48 points wide, so after spans 105..127.48 and before
        // 72.52..95.
        let points_per_base = 20.0;
        let clip = CpuClipRect {
            x: 10.0,
            y: 20.0,
            width: 1_000.0,
            height: 50.0,
        };
        let own = 100;
        let label = |kind: AlignmentLabelKind| {
            let mut label = occupant(
                AlignmentComponent::Label {
                    kind,
                    id: crate::ContractFingerprint::new(1),
                },
                99.5,
                100.5,
                own,
            );
            label.shape = CpuPrimitiveShape::TextLabel;
            label.text = Some(Arc::from("+ACGTACGTACGTAC"));
            label
        };
        let place = |occupants: &[CpuProjectedPrimitive]| {
            let mut occupancy = RowOccupancy::default();
            for occupant in occupants {
                occupancy.add(occupant);
            }
            place_insertion_label(
                label(AlignmentLabelKind::InsertionLength),
                AlignmentLabelKind::InsertionLength,
                points_per_base,
                clip,
                &occupancy,
            )
            .map(|chip| (chip.x, chip.x + chip.width, chip.text))
        };
        let (left, _, text) = place(&[]).expect("an empty row has room after the I-beam");
        assert!((left - 105.0).abs() < 1.0e-9, "{left}");
        assert_eq!(text.as_deref(), Some("+14"));
        // A short insertion has no chip, whatever room there is.
        assert!(
            place_insertion_label(
                label(AlignmentLabelKind::ShortInsertion),
                AlignmentLabelKind::ShortInsertion,
                points_per_base,
                clip,
                &RowOccupancy::default(),
            )
            .is_none()
        );

        let substitution = AlignmentComponent::Base {
            cigar_index: 0,
            read_index: 3,
            base: b'C',
            quality: Some(30),
            classification: rgv_model::AlignedBaseClassification::Substitution,
        };
        let events = [
            substitution,
            AlignmentComponent::ModifiedBase {
                read_index: 3,
                canonical_base: b'C',
                code: rgv_model::ModificationCode::Letter(b'm'),
                probability: Some(240),
                state: crate::ModificationState::Modified,
                color_by_type: true,
            },
            AlignmentComponent::Deletion { cigar_index: 2 },
            AlignmentComponent::ReferenceSkip { cigar_index: 2 },
            AlignmentComponent::Insertion {
                cigar_index: 4,
                read_start: 9,
                length: 1,
            },
            AlignmentComponent::SoftClip {
                cigar_index: 0,
                read_start: 0,
                length: 5,
            },
            AlignmentComponent::HardClip {
                cigar_index: 0,
                length: 5,
            },
            AlignmentComponent::ClipGhostBase {
                cigar_index: 0,
                read_index: 1,
                base: b'a',
                obscured: false,
            },
        ];
        let body = AlignmentComponent::AlignedSegment {
            cigar_index: 0,
            operation: crate::AlignmentOperation::Match,
        };
        for event in events {
            // On the read's own body, an event after the I-beam sends the chip before it...
            let after_event = occupant(event, 110.0, 112.0, own);
            let (_, right, _) = place(&[occupant(body, 0.0, 1_000.0, own), after_event.clone()])
                .unwrap_or_else(|| panic!("{event:?}: before has room"));
            assert!((right - 95.0).abs() < 1.0e-9, "{event:?}: {right}");
            // ...and with another before it there is no chip.
            assert_eq!(
                place(&[after_event.clone(), occupant(event, 80.0, 82.0, own)]),
                None,
                "{event:?}"
            );
            // An event of another read counts as well.
            assert!(
                place(&[occupant(event, 110.0, 112.0, own + 1)])
                    .is_some_and(|(_, right, _)| (right - 95.0).abs() < 1.0e-9),
                "{event:?}"
            );
            // Its edges are kept a point clear: half a point short of the chip still rules the
            // side out, a point and a half does not.
            assert!(
                place(&[occupant(event, 127.98, 130.0, own)])
                    .is_some_and(|(_, right, _)| (right - 95.0).abs() < 1.0e-9),
                "{event:?}"
            );
            assert!(
                place(&[occupant(event, 128.98, 130.0, own)])
                    .is_some_and(|(left, _, _)| (left - 105.0).abs() < 1.0e-9),
                "{event:?}"
            );
        }
        // Another read's body rules a side out; the own read's does not.
        assert!(
            place(&[occupant(body, 120.0, 400.0, own + 1)])
                .is_some_and(|(_, right, _)| (right - 95.0).abs() < 1.0e-9)
        );
        assert!(
            place(&[occupant(body, 120.0, 400.0, own)])
                .is_some_and(|(left, _, _)| (left - 105.0).abs() < 1.0e-9)
        );
        // Off the pane on both sides: dropped.
        assert_eq!(
            place_insertion_label(
                label(AlignmentLabelKind::InsertionLength),
                AlignmentLabelKind::InsertionLength,
                points_per_base,
                CpuClipRect {
                    x: 90.0,
                    y: 20.0,
                    width: 30.0,
                    height: 50.0,
                },
                &RowOccupancy::default(),
            ),
            None
        );
    }

    #[test]
    fn cpu_projection_minimum_size_does_not_move_the_start_edge_or_inflate_large_shapes() {
        let zoomed_out = camera(100_000, 0.25, 0.2);
        let tiny = project(
            primitive(50_000, 50_001, AlignmentGeometryToken::Rectangle),
            8,
            zoomed_out,
        )
        .unwrap();
        assert!((tiny.x - (10.0 + 50_000.0 / 1_000.0)).abs() < 1.0e-9);
        assert!((tiny.y - (20.0 + 8.0 * 0.25 + 0.025)).abs() < 1.0e-9);

        // One base per point, nine-point reads: nothing is rounded up.
        let detailed = camera(100, 12.0, 9.0);
        let body = project(
            primitive(20, 60, AlignmentGeometryToken::Rectangle),
            1,
            detailed,
        )
        .unwrap();
        assert!((body.x - 30.0).abs() < 1.0e-9);
        assert!((body.width - 40.0).abs() < 1.0e-9);
        assert!((body.height - 9.0).abs() < 1.0e-9);
    }

    #[test]
    fn cpu_projection_clips_to_the_pane_after_applying_the_minimum_size() {
        let detailed = camera(100, 12.0, 9.0);
        // Starts 30 bases left of the pane and ends 20 bases inside it.
        let projected = project_primitive(
            primitive(0, 50, AlignmentGeometryToken::Rectangle),
            970,
            0,
            detailed,
            None,
            AlignmentInteractionToken::default(),
        )
        .unwrap();
        assert!((projected.x - 10.0).abs() < 1.0e-9);
        assert!((projected.width - 20.0).abs() < 1.0e-9);

        // Wholly outside the pane: the minimum size never drags it into view.
        let outside = project_primitive(
            primitive(0, 5, AlignmentGeometryToken::Rectangle),
            900,
            0,
            detailed,
            None,
            AlignmentInteractionToken::default(),
        );
        assert!(outside.is_none());
        let empty = project(
            primitive(7, 7, AlignmentGeometryToken::Rectangle),
            0,
            detailed,
        );
        assert!(empty.is_none());
    }
}
