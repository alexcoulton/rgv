use std::{collections::HashMap, mem::size_of, ops::Range, sync::Arc};

use rgv_core::{GenomicInterval, TrackId, Viewport};
use thiserror::Error;

use crate::{
    AlignmentComponent, AlignmentGeometryToken, AlignmentHandle, AlignmentInteractionState,
    AlignmentInteractionToken, AlignmentLabelKind, AlignmentPageFamily, AlignmentPanelLaneFrame,
    AlignmentPresentationScene, AlignmentSemanticPrimitive, AlignmentStyleToken, LedgerError,
    ReservationKind, SceneRevision, TransientResourceClass, TransientResourceReservation,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuProjectedPrimitive {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub shape: CpuPrimitiveShape,
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
    pub style: AlignmentStyleToken,
    pub interaction: AlignmentInteractionToken,
    pub draw_order: u32,
    pub text: Option<Arc<str>>,
    /// Canonical atlas row. This remains stable while the row camera moves.
    pub row: u32,
}

impl CpuProjectedPrimitive {
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        if x < self.x || x >= self.x + self.width || y < self.y || y >= self.y + self.height {
            return false;
        }
        let reverse = match self.shape {
            CpuPrimitiveShape::DirectionTerminalLeft => true,
            CpuPrimitiveShape::DirectionTerminalRight => false,
            _ => return true,
        };
        let local_x = x - self.x;
        let local_y = y - self.y;
        let distance_from_tip = if reverse {
            local_x
        } else {
            self.width - local_x
        };
        let head_width = (self.height * 0.55).min(self.width * 0.35);
        if distance_from_tip >= head_width || head_width <= f64::EPSILON {
            return true;
        }
        let half_height = self.height * 0.5;
        let allowed = half_height * (distance_from_tip / head_width).clamp(0.0, 1.0);
        (local_y - half_height).abs() <= allowed
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
    _resources: Arc<CpuFrameResources>,
}

impl PartialEq for CpuAlignmentLabelFrame {
    fn eq(&self, other: &Self) -> bool {
        self.scene_revision == other.scene_revision
            && self.camera_sequence == other.camera_sequence
            && self.camera == other.camera
            && self.coverage == other.coverage
            && self.labels == other.labels
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
        let (maximum_primitives, maximum_labels) = covered_interval.map_or((0, 0), |interval| {
            scene
                .pages()
                .iter()
                .filter(|page| page.interval().overlaps(interval))
                .filter(|page| ranges_overlap(page.row_range(), covered_rows.clone()))
                .fold((0_usize, 0_usize), |(primitives, labels), page| {
                    (
                        primitives.saturating_add(page.primitives().len()),
                        labels.saturating_add(
                            page.primitives()
                                .iter()
                                .filter(|primitive| {
                                    matches!(primitive.component, AlignmentComponent::Label { .. })
                                })
                                .count(),
                        ),
                    )
                })
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
        if let Some(covered_interval) = covered_interval {
            for page in scene.visible_pages(covered_interval, covered_rows.clone()) {
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
        reconcile_labels(&mut primitives, labels.into_values(), camera.clip);
        primitives.sort_by(|left, right| {
            left.row
                .cmp(&right.row)
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
        let maximum_labels = covered_interval.map_or(0, |interval| {
            scene
                .pages()
                .iter()
                .filter(|page| page.key().family() == AlignmentPageFamily::Labels)
                .filter(|page| page.interval().overlaps(interval))
                .filter(|page| ranges_overlap(page.row_range(), covered_rows.clone()))
                .fold(0_usize, |labels, page| {
                    labels.saturating_add(
                        page.primitives()
                            .iter()
                            .filter(|primitive| {
                                matches!(primitive.component, AlignmentComponent::Label { .. })
                            })
                            .count(),
                    )
                })
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
        if let Some(covered_interval) = covered_interval {
            for page in scene
                .visible_pages(covered_interval, covered_rows.clone())
                .into_iter()
                .filter(|page| page.key().family() == AlignmentPageFamily::Labels)
            {
                for primitive in page.primitives().iter().copied().filter(|primitive| {
                    matches!(primitive.component, AlignmentComponent::Label { .. })
                }) {
                    let global_row = page.row_range().start.saturating_add(primitive.row_offset);
                    if global_row < covered_rows.start || global_row >= covered_rows.end {
                        continue;
                    }
                    let AlignmentComponent::Label { id, .. } = primitive.component else {
                        unreachable!("the iterator retains only labels");
                    };
                    if let Some(projected) = project_primitive(
                        primitive,
                        page.interval().start(),
                        global_row,
                        camera,
                        page.label_arc(id),
                        interaction.token(primitive.handle),
                    ) {
                        fragments
                            .entry((projected.handle, projected.component))
                            .and_modify(|existing| merge_label_fragments(existing, &projected))
                            .or_insert(projected);
                    }
                }
            }
        }
        let mut labels = Vec::with_capacity(maximum_labels);
        reconcile_labels(&mut labels, fragments.into_values(), camera.clip);
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
        Ok(pick_captured_scene(
            snapshot.reads(),
            lane.track_id,
            lane.camera,
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
    scene
        .pages()
        .iter()
        .filter(|page| page.interval().contains(position))
        .flat_map(|page| {
            page.primitive_candidates(row, position)
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
        .max_by_key(|primitive| (primitive.draw_order, primitive.component, primitive.handle))
        .map(|primitive| AlignmentHit {
            track,
            scene_revision: scene.revision(),
            handle: primitive.handle,
            component: primitive.component,
        })
}

fn project_primitive(
    primitive: AlignmentSemanticPrimitive,
    page_start: u64,
    global_row: u32,
    camera: AlignmentCamera,
    text: Option<Arc<str>>,
    interaction: AlignmentInteractionToken,
) -> Option<CpuProjectedPrimitive> {
    let genomic_start = page_start.saturating_add(primitive.start_offset);
    let genomic_end = page_start.saturating_add(primitive.end_offset);
    let clipped_start = genomic_start.max(camera.interval().start());
    let clipped_end = genomic_end.min(camera.interval().end());
    if clipped_start >= clipped_end {
        return None;
    }
    let mut x = camera.x_for_position(clipped_start);
    let mut width = camera.x_for_position(clipped_end) - x;
    let row_top = camera.clip.y
        + (f64::from(global_row) - camera.row_scroll) * camera.row_height
        + (camera.row_height - camera.read_height) * 0.5;
    let (shape, y, height) = match primitive.geometry {
        AlignmentGeometryToken::EventLine => (
            CpuPrimitiveShape::EventLine,
            row_top + camera.read_height * 0.45,
            (camera.read_height * 0.1).max(1.0),
        ),
        AlignmentGeometryToken::PointMarker => {
            let marker_width = width.max(1.5);
            x -= (marker_width - width) * 0.5;
            width = marker_width;
            (CpuPrimitiveShape::PointMarker, row_top, camera.read_height)
        }
        AlignmentGeometryToken::DirectionalTerminalLeft => {
            width = width.max(1.0);
            (
                CpuPrimitiveShape::DirectionTerminalLeft,
                row_top,
                camera.read_height,
            )
        }
        AlignmentGeometryToken::DirectionalTerminalRight => {
            width = width.max(1.0);
            (
                CpuPrimitiveShape::DirectionTerminalRight,
                row_top,
                camera.read_height,
            )
        }
        AlignmentGeometryToken::Rectangle => {
            let shape = match primitive.component {
                AlignmentComponent::Label { .. } => CpuPrimitiveShape::TextLabel,
                AlignmentComponent::Base { .. }
                | AlignmentComponent::ModifiedBase { .. }
                | AlignmentComponent::BisulfiteBase { .. }
                | AlignmentComponent::SmrtKinetics { .. }
                | AlignmentComponent::SbxTailBase { .. } => CpuPrimitiveShape::BaseCell,
                _ => CpuPrimitiveShape::ReadBody,
            };
            (shape, row_top, camera.read_height)
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
        handle: primitive.handle,
        component: primitive.component,
        style: primitive.style,
        interaction,
        draw_order: primitive.draw_order,
        text,
        row: global_row,
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

fn reconcile_labels(
    primitives: &mut Vec<CpuProjectedPrimitive>,
    labels: impl Iterator<Item = CpuProjectedPrimitive>,
    clip: CpuClipRect,
) {
    const MIN_TEXT_HEIGHT: f64 = 5.0;
    const HORIZONTAL_PADDING: f64 = 2.0;
    const COLLISION_GAP: f64 = 2.0;
    const GLYPH_ASPECT_RATIO: f64 = 0.56;

    let clip_right = clip.x + clip.width;
    let mut candidates = labels
        .filter_map(|mut label| {
            let text = label.text.as_deref()?;
            if label.height < MIN_TEXT_HEIGHT {
                return None;
            }
            let glyph_count = text.chars().count().max(1) as f64;
            let text_width =
                glyph_count * label.height * GLYPH_ASPECT_RATIO + HORIZONTAL_PADDING * 2.0;
            if text_width > clip.width {
                return None;
            }
            let must_fit_anchor = matches!(
                label.component,
                AlignmentComponent::Label {
                    kind: AlignmentLabelKind::ReadName | AlignmentLabelKind::AuxiliaryTag(_),
                    ..
                }
            );
            if must_fit_anchor && text_width > label.width {
                return None;
            }
            let anchor = label.x + label.width * 0.5;
            label.x = (anchor - text_width * 0.5).clamp(clip.x, clip_right - text_width);
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

const fn ranges_overlap(left: Range<u32>, right: Range<u32>) -> bool {
    left.start < right.end && left.end > right.start
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
