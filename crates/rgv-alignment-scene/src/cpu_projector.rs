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

    fn x_for_position(self, position: u64) -> f64 {
        self.clip.x + self.viewport.pixel_at_genomic(position)
    }

    fn position_for_x(self, x: f64) -> Option<u64> {
        if x < self.clip.x || x >= self.clip.x + self.clip.width {
            return None;
        }
        let position = self.viewport.genomic_at_pixel(x - self.clip.x).floor() as u64;
        Some(position.clamp(
            self.interval().start(),
            self.interval().end().saturating_sub(1),
        ))
    }

    fn row_for_y(self, y: f64) -> Option<u32> {
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
    /// shapes: described in unclipped pane coordinates, so a primitive clipped by the pane keeps
    /// the read's own tip and diagonal rather than growing a point at the pane edge.
    pub arrow: Option<CpuArrowhead>,
}

impl CpuProjectedPrimitive {
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        if x < self.x || x >= self.x + self.width || y < self.y || y >= self.y + self.height {
            return false;
        }
        self.arrow.is_none_or(|arrow| arrow.covers(x, y))
    }
}

/// The single direction arrowhead at a read's true terminal (shapes 3 and 4 of `fragment_main`
/// in `rgv-render`'s `alignment_scene.wgsl`; keep in lockstep). Every primitive at the tip (the
/// aligned block, the one-base direction terminal and, at base detail, the base cell) ends on
/// the read's tip and is cut by this same diagonal, so the read's outline is one point at every
/// zoom: a primitive narrower than the head is trimmed, never re-pointed at its own scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuArrowhead {
    /// Pane x of the read's tip: its right end when `points_right`, else its left end.
    pub tip_x: f64,
    /// Pane y of the read's centre line.
    pub centre_y: f64,
    /// Unclipped height of the read, in points (at least one point).
    pub height: f64,
    pub points_right: bool,
}

impl CpuArrowhead {
    /// Length of the head along the read; see [`arrowhead_length`].
    #[must_use]
    pub fn length(self) -> f64 {
        arrowhead_length(self.height)
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
        arrowhead_covers(self.distance_from_tip(x), y - self.centre_y, self.height)
    }
}

/// The direction arrowhead's length along the read, for a read drawn `height` points tall:
/// [`ARROWHEAD_LENGTH_FRACTION`] of the height, and nothing else. It never depends on the width
/// of the primitive being cut, so the block, the one-base terminal and a base cell at the tip
/// share one diagonal; a read shorter on screen than this is drawn as (part of) the arrow.
#[must_use]
pub fn arrowhead_length(height: f64) -> f64 {
    height * ARROWHEAD_LENGTH_FRACTION
}

/// Whether a point `distance_from_tip` into the read from its tip and `offset_from_centre`
/// from its centre line is inside the arrow outline of a read `height` points tall: within the
/// head the half-height allowed grows linearly from zero at the tip to the full half-height at
/// [`arrowhead_length`]. Mirrors `fragment_main` in `alignment_scene.wgsl`.
#[must_use]
pub fn arrowhead_covers(distance_from_tip: f64, offset_from_centre: f64, height: f64) -> bool {
    let length = arrowhead_length(height);
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
        reconcile_labels(&mut primitives, labels.into_values(), camera);
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

    /// Projects only camera-dependent text. The scan is bounded to intersecting semantic pages
    /// and row blocks, and its exact temporary capacity is admitted through the scene ledger
    /// before allocation.
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
        if covered_interval.is_some() {
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
        reconcile_labels(&mut labels, fragments.into_values(), camera);
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
    // A point marker is centred on a base boundary and overhangs by at most half its maximum
    // width, and a clip edge lies at most an arrowhead plus its own width inside its boundary,
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
    scene
        .visible_pages(search_interval, row..row.saturating_add(1))
        .into_iter()
        .flat_map(|page| {
            page.pick_candidates(row, position, marker_start..marker_end)
                .filter(|primitive| {
                    !matches!(primitive.component, AlignmentComponent::Label { .. })
                })
                .filter_map(|primitive| {
                    project_primitive(
                        primitive,
                        page.interval().start(),
                        row,
                        camera,
                        match primitive.component {
                            AlignmentComponent::Label { id, .. } => page.label_arc(id),
                            _ => None,
                        },
                        AlignmentInteractionToken::default(),
                    )
                })
        })
        .filter(|primitive| primitive.contains(x, y))
        // Boundary marks paint over their row and clip ghosts beneath it (see the semantic page
        // order), so the paint layer decides here too.
        .max_by_key(|primitive| {
            (
                crate::semantic_page::primitive_paint_layer(
                    primitive.component,
                    primitive.geometry,
                ),
                primitive.draw_order,
                primitive.component,
                primitive.handle,
            )
        })
        .map(|primitive| AlignmentHit {
            track,
            scene_revision: scene.revision(),
            handle: primitive.handle,
            component: primitive.component,
        })
}

/// Smallest extent, in points, any projected alignment shape may have along either axis. One
/// point is at least one physical pixel at every window scale factor. Keep in lockstep with
/// `project_instance` in `rgv-render`'s `alignment_scene.wgsl`.
const MINIMUM_PRIMITIVE_EXTENT: f64 = 1.0;

/// Point markers (insertions, clips, padding, specialist insertions) are narrow bars centred on
/// a base boundary: a quarter of a base wide, but never narrower than two points (so they stay
/// visible zoomed out) nor wider than six (so they never read as a base cell). Keep in lockstep
/// with shapes 2 and 5 of `project_instance` in `rgv-render`'s `alignment_scene.wgsl` and with
/// `project_alignment_gpu_instance`.
const POINT_MARKER_BASE_FRACTION: f64 = 0.25;
const POINT_MARKER_MINIMUM_WIDTH: f64 = 2.0;
const POINT_MARKER_MAXIMUM_WIDTH: f64 = 6.0;

/// Horizontal extent `(left, width)` of a point marker whose one-base span projects to
/// `cell_left..cell_right`: centred on the cell's start edge, or on its end edge when `at_end`
/// (`AlignmentGeometryToken::PointMarkerAtEnd`).
#[must_use]
pub fn point_marker_extent(cell_left: f64, cell_right: f64, at_end: bool) -> (f64, f64) {
    let width = ((cell_right - cell_left) * POINT_MARKER_BASE_FRACTION)
        .clamp(POINT_MARKER_MINIMUM_WIDTH, POINT_MARKER_MAXIMUM_WIDTH);
    let boundary = if at_end { cell_right } else { cell_left };
    (boundary - width * 0.5, width)
}

/// Clip cut edges (shapes 6..=13 of `project_instance` and `fragment_main` in `rgv-render`'s
/// `alignment_scene.wgsl`, mirrored by `project_alignment_gpu_instance`; keep in lockstep). A
/// soft clip's sawtooth is half a base wide within [2, 6] points; a hard clip's bar is two.
const SOFT_CLIP_EDGE_BASE_FRACTION: f64 = 0.5;
const SOFT_CLIP_EDGE_MINIMUM_WIDTH: f64 = 2.0;
const SOFT_CLIP_EDGE_MAXIMUM_WIDTH: f64 = 6.0;
const HARD_CLIP_EDGE_WIDTH: f64 = 2.0;
/// Stroke of the sawtooth, measured across the stroke, and the target height of one tooth.
const CLIP_EDGE_STROKE: f64 = 1.5;
const CLIP_EDGE_TOOTH_HEIGHT: f64 = 4.0;
/// The direction arrowhead of `fragment_main` (shapes 3 and 4): its length along the read is
/// this fraction of the read height; see [`arrowhead_length`].
pub const ARROWHEAD_LENGTH_FRACTION: f64 = 0.55;

/// Horizontal extent `(left, width)` of a clip's cut edge whose one-base span (the aligned base
/// at the clipped boundary) projects to `cell_left..cell_right`, for a read drawn `height`
/// points tall. The edge lies just inside the read: its outer side is the boundary (the cell's
/// start edge, or its end edge when `edge.at_end`), so it never covers a ghost base beyond it.
///
/// When the read's direction arrowhead is on the same boundary (`edge.behind_arrow`) the edge
/// is moved inward from that boundary (the read's tip) by exactly the arrowhead's length,
/// [`arrowhead_length`] of `height`. The edge then starts where the arrowhead's base is, so
/// tip and cut both stay visible.
#[must_use]
pub fn clip_edge_extent(
    cell_left: f64,
    cell_right: f64,
    height: f64,
    edge: ClipEdgeToken,
) -> (f64, f64) {
    let cell = cell_right - cell_left;
    let width = if edge.hard {
        HARD_CLIP_EDGE_WIDTH
    } else {
        (cell * SOFT_CLIP_EDGE_BASE_FRACTION)
            .clamp(SOFT_CLIP_EDGE_MINIMUM_WIDTH, SOFT_CLIP_EDGE_MAXIMUM_WIDTH)
    };
    if !edge.behind_arrow {
        return if edge.at_end {
            (cell_right - width, width)
        } else {
            (cell_left, width)
        };
    }
    // Direction terminals keep their tip on the read's boundary (see `directional_extent`).
    let arrowhead = arrowhead_length(height);
    if edge.at_end {
        (cell_right - arrowhead - width, width)
    } else {
        (cell_left + arrowhead, width)
    }
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

fn project_primitive(
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
    let mut arrow = None;
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
                // A boundary-anchored label keeps its one-base box (so its visibility rules are
                // unchanged) but centres it on the boundary its event's marker is drawn on.
                let boundary = if at_end { x + width } else { x };
                width = width.max(MINIMUM_PRIMITIVE_EXTENT);
                x = boundary - width * 0.5;
                (CpuPrimitiveShape::TextLabel, row_top, body_height)
            } else {
                (x, width) = point_marker_extent(x, x + width, at_end);
                (CpuPrimitiveShape::PointMarker, row_top, body_height)
            }
        }
        AlignmentGeometryToken::ClipEdge(edge) => {
            if matches!(primitive.component, AlignmentComponent::Label { .. }) {
                // A clip length keeps its anchor span; `reconcile_labels` places the text
                // outside the read, beyond that span.
                width = width.max(MINIMUM_PRIMITIVE_EXTENT);
                (CpuPrimitiveShape::TextLabel, row_top, body_height)
            } else {
                (x, width) = clip_edge_extent(x, x + width, body_height, edge);
                (CpuPrimitiveShape::ClipEdge, row_top, body_height)
            }
        }
        AlignmentGeometryToken::ClipGhost => {
            width = width.max(MINIMUM_PRIMITIVE_EXTENT);
            (CpuPrimitiveShape::ClipGhostCell, row_top, body_height)
        }
        AlignmentGeometryToken::DirectionalTerminalLeft => {
            (x, width) = directional_extent(x, x + width, false);
            arrow = Some(CpuArrowhead {
                tip_x: x,
                centre_y: row_top + body_height * 0.5,
                height: body_height,
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
                tip_x: x + width,
                centre_y: row_top + body_height * 0.5,
                height: body_height,
                points_right: true,
            });
            (
                CpuPrimitiveShape::DirectionTerminalRight,
                row_top,
                body_height,
            )
        }
        AlignmentGeometryToken::Rectangle => {
            width = width.max(MINIMUM_PRIMITIVE_EXTENT);
            let shape = match primitive.component {
                AlignmentComponent::Label { .. } => CpuPrimitiveShape::TextLabel,
                AlignmentComponent::Base { .. }
                | AlignmentComponent::ModifiedBase { .. }
                | AlignmentComponent::BisulfiteBase { .. }
                | AlignmentComponent::SmrtKinetics { .. }
                | AlignmentComponent::SbxTailBase { .. } => CpuPrimitiveShape::BaseCell,
                _ => CpuPrimitiveShape::ReadBody,
            };
            (shape, row_top, body_height)
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
    })
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
/// * a point event (insertion, clip) has no on-screen extent, so its length shows only from
///   this many points per base, and only when the text fits in the width its `length` unseen
///   bases would occupy at the current zoom (`length x points per base`).
pub const EVENT_LABEL_MINIMUM_POINTS_PER_BASE: f64 = 2.0;

/// The event length a point-event label states: its leading decimal digits (a hard clip's
/// length carries an `H` suffix). The page builder writes these texts, so they always parse.
fn point_event_label_length(text: &str) -> f64 {
    let digits = text.trim_end_matches(|character: char| !character.is_ascii_digit());
    digits.parse::<u32>().map_or(1.0, f64::from).max(1.0)
}

fn reconcile_labels(
    primitives: &mut Vec<CpuProjectedPrimitive>,
    labels: impl Iterator<Item = CpuProjectedPrimitive>,
    camera: AlignmentCamera,
) {
    const MIN_TEXT_HEIGHT: f64 = 5.0;
    const MINIMUM_BASE_GLYPH_WIDTH: f64 = 7.0;
    const HORIZONTAL_PADDING: f64 = 2.0;
    const COLLISION_GAP: f64 = 2.0;
    const GLYPH_ASPECT_RATIO: f64 = 0.56;

    let clip = camera.clip;
    let points_per_base = 1.0 / camera.bases_per_pixel();
    let clip_right = clip.x + clip.width;
    let mut base_labels = Vec::new();
    let mut candidates = labels
        .filter_map(|mut label| {
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
            let glyph_count = text.chars().count().max(1) as f64;
            let text_width =
                glyph_count * label.height * GLYPH_ASPECT_RATIO + HORIZONTAL_PADDING * 2.0;
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
                    kind: AlignmentLabelKind::InsertionLength | AlignmentLabelKind::ClipLength,
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
                // A clip length sits outside the read, just beyond its anchor (the clipped
                // boundary and any ghost bases drawn past it), never over the read body: when
                // that place is off the pane the label is dropped rather than moved inwards.
                let left = if edge.at_end {
                    label.x + label.width
                } else {
                    label.x - text_width
                };
                if left < clip.x || left + text_width > clip_right {
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

    primitives.append(&mut base_labels);

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

    const GEOMETRIES: [AlignmentGeometryToken; 9] = [
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
            let projected = project(primitive(50_000, 50_001, geometry), 8, camera)
                .unwrap_or_else(|| panic!("{geometry:?} vanished below one point"));
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
        // Point markers keep a two-point minimum (it was 1.5 when a marker filled its base cell)
        // so a boundary bar stays visible when zoomed out, centred on its boundary.
        for (geometry, boundary) in [
            (AlignmentGeometryToken::PointMarker, 50_000.0),
            (AlignmentGeometryToken::PointMarkerAtEnd, 50_001.0),
        ] {
            let marker = project(primitive(50_000, 50_001, geometry), 8, camera).unwrap();
            assert!((marker.width - 2.0).abs() < 1.0e-9);
            assert!((marker.x + 1.0 - (10.0 + boundary / 1_000.0)).abs() < 1.0e-9);
        }
    }

    #[test]
    fn point_markers_are_narrow_bars_on_a_base_boundary_and_labels_follow_them() {
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
            let marker = project(primitive(5, 6, geometry), 0, camera).unwrap();
            assert_eq!(marker.shape, CpuPrimitiveShape::PointMarker);
            assert!((marker.width - 6.0).abs() < 1.0e-9, "{marker:?}");
            assert!((marker.x + 3.0 - boundary).abs() < 1.0e-9, "{marker:?}");
            assert!((marker.height - 11.0).abs() < 1.0e-9);

            // A label carrying the same token keeps its one-base box, centred on the boundary.
            let mut label = primitive(5, 6, geometry);
            label.component = AlignmentComponent::Label {
                kind: AlignmentLabelKind::InsertionLength,
                id: crate::ContractFingerprint::new(1),
            };
            let label = project(label, 0, camera).unwrap();
            assert_eq!(label.shape, CpuPrimitiveShape::TextLabel);
            assert!((label.width - 112.0).abs() < 1.0e-9);
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
