//! Software export of the same immutable scene and cameras used by native
//! presentation. No source IO or second biological interpretation lives here.
use rgv_alignment_scene::{
    AlignmentDerivedLaneOptions, AlignmentPanelSceneFrame, AlignmentSashimiScalePolicy,
    BisulfiteObservationToken, CpuAlignmentProjector, CpuAlleleToken, CpuJunctionStrandToken,
    CpuPrimitiveShape, CpuSummaryCamera, CpuSummaryPrimitiveShape, CpuSummaryProjectedComponent,
    CpuSummaryProjectionOptions, CpuSummaryProjector, JunctionAnnotationStatus, ReservationKind,
    TransientResourceClass, TransientResourceReservation,
};
use rgv_core::ModifiedBaseColorMode;
use rgv_layout::{DisplayList, RectPrimitive, RectShape, Rgba, modification_code_color};
use std::{collections::HashMap, io};

use crate::palette::{AlignmentPalette, PaletteToken};

pub struct SemanticAlignmentExport {
    pub display: DisplayList,
    // Charge the retained adapter output through encoding, including a cropped copy.
    resources: Vec<TransientResourceReservation>,
}

fn export_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

impl SemanticAlignmentExport {
    fn reserve(
        &mut self,
        ledger: &rgv_alignment_scene::AllocationLedger,
        count: usize,
    ) -> io::Result<()> {
        let bytes = count
            // Includes three rectangle copies and bounded SVG formatting/
            // encoder capacity, not just the final display-list allocation.
            .checked_mul(1024)
            .ok_or_else(|| export_error("semantic export size overflow"))?;
        self.resources.push(
            ledger
                .reserve_transient(
                    TransientResourceClass::CpuProjection,
                    bytes,
                    ReservationKind::Normal,
                )
                .map_err(export_error)?,
        );
        self.display
            .rectangles
            .try_reserve_exact(count)
            .map_err(export_error)
    }
}

/// Call from an export worker with captured, committed panel frames. Capturing
/// those frames pins the source/reference/style authority for the job lifetime. Colours come
/// from `palette`, the same table the GPU renderers resolve their instances against.
#[allow(clippy::too_many_lines)]
pub fn project_semantic_alignment_export<S: std::hash::BuildHasher>(
    panels: &[AlignmentPanelSceneFrame],
    ledger: &rgv_alignment_scene::AllocationLedger,
    interactions: &HashMap<
        (rgv_core::PanelId, rgv_core::TrackId),
        rgv_alignment_scene::AlignmentInteractionState,
        S,
    >,
    palette: &AlignmentPalette,
) -> io::Result<SemanticAlignmentExport> {
    let mut output = SemanticAlignmentExport {
        display: DisplayList::default(),
        resources: Vec::new(),
    };
    for panel in panels {
        let mut shared_scales = HashMap::new();
        for lane in panel.lanes.iter() {
            for derived in lane.derived_lanes.iter() {
                if let AlignmentDerivedLaneOptions::Sashimi(options) = derived.options
                    && let AlignmentSashimiScalePolicy::Shared(group) = options.scale
                    && let Some(scene) = &lane.snapshot
                {
                    let scale = CpuSummaryProjector::measure_junction_scale(
                        scene,
                        lane.preparation_camera.interval(),
                        options.projection,
                    )
                    .map_err(export_error)?
                    .maximum;
                    shared_scales
                        .entry(group)
                        .and_modify(|max: &mut f64| *max = max.max(scale))
                        .or_insert(scale);
                }
            }
        }
        for lane in panel.lanes.iter() {
            if !lane.visible_complete() {
                return Err(export_error("committed alignment evidence is incomplete"));
            }
            let scene = lane
                .snapshot
                .as_ref()
                .ok_or_else(|| export_error("committed alignment scene is unavailable"))?;
            // Native draw order is summaries, reads, then generic text.
            for derived in lane.derived_lanes.iter() {
                let camera = CpuSummaryCamera::from_viewport(
                    lane.preparation_camera.viewport(),
                    derived.clip,
                )
                .map_err(export_error)?;
                let mut projections = Vec::new();
                match &derived.options {
                    AlignmentDerivedLaneOptions::Coverage(options) => {
                        projections.push(
                            CpuSummaryProjector::project(
                                scene,
                                lane.source_camera_sequence,
                                camera,
                                &CpuSummaryProjectionOptions::Depth(options.depth),
                            )
                            .map_err(export_error)?,
                        );
                        if let Some(mut alleles) = options.alleles {
                            alleles.scale_maximum =
                                projections[0].scale_maximum.map(|scale| scale as u32);
                            projections.push(
                                CpuSummaryProjector::project(
                                    scene,
                                    lane.source_camera_sequence,
                                    camera,
                                    &CpuSummaryProjectionOptions::Alleles(alleles),
                                )
                                .map_err(export_error)?,
                            );
                        }
                    }
                    AlignmentDerivedLaneOptions::Sashimi(options) => {
                        let mut projection = options.projection;
                        projection.scale_maximum = match options.scale {
                            AlignmentSashimiScalePolicy::Automatic => None,
                            AlignmentSashimiScalePolicy::Fixed(maximum) => Some(maximum),
                            AlignmentSashimiScalePolicy::Shared(group) => {
                                shared_scales.get(&group).copied()
                            }
                        };
                        projections.push(
                            CpuSummaryProjector::project(
                                scene,
                                lane.source_camera_sequence,
                                camera,
                                &CpuSummaryProjectionOptions::Junctions(projection),
                            )
                            .map_err(export_error)?,
                        );
                    }
                    AlignmentDerivedLaneOptions::Modifications(options) => {
                        projections.push(
                            CpuSummaryProjector::project_dominant_modifications(
                                scene,
                                lane.source_camera_sequence,
                                camera,
                                &options.projection,
                            )
                            .map_err(export_error)?,
                        );
                    }
                }
                for projected in projections {
                    if !projected.normalization_available {
                        return Err(export_error("summary normalization is unavailable"));
                    }
                    let modification_background = matches!(&derived.options,
                        AlignmentDerivedLaneOptions::Modifications(options) if options.color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified);
                    let clip = camera.clip();
                    // Arc bounds are unclamped; the visible part of an arc that leaves the lane
                    // is exported as thin slices, which need their own reservation.
                    let arc_slices = projected
                        .primitives
                        .iter()
                        .map(|primitive| clipped_arc_slice_count(primitive, clip))
                        .fold(0_usize, usize::saturating_add);
                    output.reserve(
                        ledger,
                        projected
                            .primitives
                            .len()
                            .saturating_mul(if modification_background { 2 } else { 1 })
                            .saturating_add(arc_slices),
                    )?;
                    if modification_background {
                        for primitive in projected.primitives.iter() {
                            output.display.rectangles.push(RectPrimitive {
                                x: primitive.x as f32,
                                y: camera.clip().y as f32,
                                width: primitive.width as f32,
                                height: camera.clip().height as f32,
                                color: rgba(
                                    palette.color(PaletteToken::SUMMARY_MODIFICATION_BACKGROUND),
                                ),
                                object_id: 0,
                                shape: RectShape::Rectangle,
                            });
                        }
                    }
                    for primitive in projected
                        .primitives
                        .iter()
                        .filter(|p| p.shape != CpuSummaryPrimitiveShape::TextLabel)
                    {
                        if clipped_arc_slice_count(primitive, clip) > 0 {
                            push_clipped_arc_slices(
                                &mut output.display.rectangles,
                                primitive,
                                clip,
                                summary_color(primitive.component, &derived.options, palette),
                            );
                            continue;
                        }
                        output.display.rectangles.push(RectPrimitive {
                            x: primitive.x as f32,
                            y: primitive.y as f32,
                            width: primitive.width as f32,
                            height: primitive.height as f32,
                            color: summary_color(primitive.component, &derived.options, palette),
                            object_id: 0,
                            shape: match primitive.shape {
                                CpuSummaryPrimitiveShape::JunctionArcUp => RectShape::JunctionArcUp,
                                CpuSummaryPrimitiveShape::JunctionArcDown => {
                                    RectShape::JunctionArcDown
                                }
                                _ => RectShape::Rectangle,
                            },
                        });
                    }
                }
            }
            if let Some(camera) = lane.read_camera {
                let default_interaction = rgv_alignment_scene::AlignmentInteractionState::default();
                let frame = CpuAlignmentProjector::project_with_interaction(
                    scene.reads(),
                    lane.source_camera_sequence,
                    camera,
                    interactions
                        .get(&(panel.panel_id, lane.track_id))
                        .unwrap_or(&default_interaction),
                )
                .map_err(export_error)?;
                // A soft clip's sawtooth is exported as horizontal slices of its zigzag.
                let sawtooth_slices = frame
                    .primitives
                    .iter()
                    .map(sawtooth_slice_count)
                    .fold(0_usize, usize::saturating_add);
                // A read's arrowhead is exported as its exact outline: a body rectangle plus a
                // triangle, or slices where the pane clips the read.
                let arrow_parts = frame
                    .primitives
                    .iter()
                    .map(directional_part_count)
                    .fold(0_usize, usize::saturating_add);
                output.reserve(
                    ledger,
                    frame
                        .primitives
                        .len()
                        .saturating_add(sawtooth_slices)
                        .saturating_add(arrow_parts),
                )?;
                for primitive in frame
                    .primitives
                    .iter()
                    .filter(|p| p.shape != CpuPrimitiveShape::TextLabel)
                {
                    let color = palette.resolve(crate::alignment_scene::semantic_alignment_color(
                        primitive.component,
                        primitive.style,
                    ));
                    let mut color = palette.interaction_color(color, primitive.interaction);
                    if primitive.shape == CpuPrimitiveShape::ClipGhostCell {
                        color = palette.clip_ghost_color(color);
                    }
                    let [r, g, b, a] = color;
                    if sawtooth_slice_count(primitive) > 0 {
                        push_sawtooth_slices(
                            &mut output.display.rectangles,
                            primitive,
                            Rgba::new(r, g, b, a),
                        );
                        continue;
                    }
                    if let Some(arrow) = primitive.arrow {
                        directional_parts(primitive, arrow, |part| {
                            output.display.rectangles.push(RectPrimitive {
                                x: part.x as f32,
                                y: part.y as f32,
                                width: part.width as f32,
                                height: part.height as f32,
                                color: Rgba::new(r, g, b, a),
                                object_id: 0,
                                shape: part.shape,
                            });
                        });
                        continue;
                    }
                    output.display.rectangles.push(RectPrimitive {
                        x: primitive.x as f32,
                        y: primitive.y as f32,
                        width: primitive.width as f32,
                        height: primitive.height as f32,
                        color: Rgba::new(r, g, b, a),
                        object_id: 0,
                        shape: RectShape::Rectangle,
                    });
                }
            }
        }
    }
    Ok(output)
}

/// One display rectangle of a primitive cut by its read's arrowhead.
pub(crate) struct DirectionalPart {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) shape: RectShape,
}

/// Height, in points, of the slices that trace an arrowhead the pane clips.
const ARROW_SLICE_HEIGHT: f64 = 0.5;
/// Tolerance, in points, for "this edge is the read's own tip / full height".
const ARROW_EDGE_TOLERANCE: f64 = 1.0e-6;

/// Number of display rectangles [`directional_parts`] emits for `primitive`; zero unless it is
/// cut by its read's arrowhead.
fn directional_part_count(primitive: &rgv_alignment_scene::CpuProjectedPrimitive) -> usize {
    let mut count = 0_usize;
    if let Some(arrow) = primitive.arrow {
        directional_parts(primitive, arrow, |_| count += 1);
    }
    count
}

/// Exports a primitive at a read's tip as exactly the outline the GPU's `fragment_main` paints
/// (`rgv_alignment_scene::arrowhead_covers`): every tip primitive is cut by the read's one
/// diagonal, whose length depends on the read height and the camera scale alone
/// (`rgv_alignment_scene::arrowhead_length`), measured from the read's tip. A zero-length head
/// (far zoomed out) leaves a plain rectangle.
///
/// The display list's `DirectionalTerminal` shapes size their head from the rectangle's own
/// width (other tracks rely on that) and `rgv-layout` is outside this change, so the cut is
/// written out: the part beyond the head is a plain rectangle, and the part within it is a
/// chevron (a triangle whose head is its whole width) as tall as the diagonal is at its base.
/// Where the pane clips the read, so the tip or the full height is not in view, the head is
/// traced with thin horizontal slices instead.
pub(crate) fn directional_parts(
    primitive: &rgv_alignment_scene::CpuProjectedPrimitive,
    arrow: rgv_alignment_scene::CpuArrowhead,
    mut emit: impl FnMut(DirectionalPart),
) {
    let length = arrow.length();
    let left_distance = arrow.distance_from_tip(primitive.x);
    let right_distance = arrow.distance_from_tip(primitive.x + primitive.width);
    let (near, far) = (
        left_distance.min(right_distance),
        left_distance.max(right_distance),
    );
    // The horizontal extent between two distances from the tip.
    let span = |from: f64, to: f64| {
        if arrow.points_right {
            (arrow.tip_x - to, to - from)
        } else {
            (arrow.tip_x + from, to - from)
        }
    };
    if far > length {
        let (x, width) = span(near.max(length), far);
        emit(DirectionalPart {
            x,
            y: primitive.y,
            width,
            height: primitive.height,
            shape: RectShape::Rectangle,
        });
    }
    let head_end = far.min(length);
    if near >= head_end {
        return;
    }
    let top = arrow.centre_y - arrow.height * 0.5;
    let whole = near.abs() <= ARROW_EDGE_TOLERANCE
        && (primitive.y - top).abs() <= ARROW_EDGE_TOLERANCE
        && (primitive.height - arrow.height).abs() <= ARROW_EDGE_TOLERANCE;
    if whole {
        let (x, width) = span(0.0, head_end);
        let height = arrow.height * head_end / length;
        emit(DirectionalPart {
            x,
            y: arrow.centre_y - height * 0.5,
            width,
            height,
            shape: if arrow.points_right {
                RectShape::ChevronRight
            } else {
                RectShape::ChevronLeft
            },
        });
        return;
    }
    let mut slice_top = primitive.y;
    let bottom = primitive.y + primitive.height;
    while slice_top < bottom {
        let slice_bottom = (slice_top + ARROW_SLICE_HEIGHT).min(bottom);
        if let Some(outline) = arrow.outline_distance_at((slice_top + slice_bottom) * 0.5) {
            let from = near.max(outline);
            if from < head_end {
                let (x, width) = span(from, head_end);
                emit(DirectionalPart {
                    x,
                    y: slice_top,
                    width,
                    height: slice_bottom - slice_top,
                    shape: RectShape::Rectangle,
                });
            }
        }
        slice_top = slice_bottom;
    }
}

/// Height, in points, of the slices that trace a soft clip's sawtooth cut edge.
const SAWTOOTH_SLICE_HEIGHT: f64 = 0.5;
/// The painted columns of one sawtooth scanline form one run, found on a grid this fine.
const SAWTOOTH_SCAN_STEPS: usize = 64;

/// Number of display rectangles [`push_sawtooth_slices`] emits for `primitive`; zero unless it
/// is a soft clip's cut edge.
fn sawtooth_slice_count(primitive: &rgv_alignment_scene::CpuProjectedPrimitive) -> usize {
    match primitive.geometry {
        rgv_alignment_scene::AlignmentGeometryToken::ClipEdge(edge)
            if !edge.hard && primitive.shape == CpuPrimitiveShape::ClipEdge =>
        {
            (primitive.height / SAWTOOTH_SLICE_HEIGHT).ceil().max(1.0) as usize
        }
        _ => 0,
    }
}

/// Exports a soft clip's zigzag cut edge as thin horizontal slices, each spanning the columns
/// `rgv_alignment_scene::clip_edge_covers` paints at the slice's mid-height (the GPU's
/// `fragment_main` rule), clipped to the edge's own extent. The display list has no shape for a
/// zigzag, and `rgv-layout` is outside this change.
fn push_sawtooth_slices(
    rectangles: &mut Vec<RectPrimitive>,
    primitive: &rgv_alignment_scene::CpuProjectedPrimitive,
    color: Rgba,
) {
    let rgv_alignment_scene::AlignmentGeometryToken::ClipEdge(edge) = primitive.geometry else {
        return;
    };
    let mut top = primitive.y;
    let bottom = primitive.y + primitive.height;
    while top < bottom {
        let slice_bottom = (top + SAWTOOTH_SLICE_HEIGHT).min(bottom);
        let local_y = (top + slice_bottom) * 0.5 - primitive.y;
        let mut run: Option<(f64, f64)> = None;
        for step in 0..SAWTOOTH_SCAN_STEPS {
            let local_x = (step as f64 + 0.5) / SAWTOOTH_SCAN_STEPS as f64 * primitive.width;
            if rgv_alignment_scene::clip_edge_covers(
                local_x,
                local_y,
                primitive.width,
                primitive.height,
                edge,
            ) {
                let cell = primitive.width / SAWTOOTH_SCAN_STEPS as f64;
                let (left, right) = (local_x - cell * 0.5, local_x + cell * 0.5);
                run = Some(run.map_or((left, right), |(start, end)| {
                    (start.min(left), end.max(right))
                }));
            }
        }
        if let Some((left, right)) = run {
            rectangles.push(RectPrimitive {
                x: (primitive.x + left) as f32,
                y: top as f32,
                width: (right - left) as f32,
                height: (slice_bottom - top) as f32,
                color,
                object_id: 0,
                shape: RectShape::Rectangle,
            });
        }
        top = slice_bottom;
    }
}

/// Width, in points, of the slices that stand in for a junction arc leaving its lane.
const CLIPPED_ARC_SLICE_WIDTH: f64 = 0.5;

/// The part of a junction arc's horizontal extent that lies inside the lane clip, if the arc
/// extends beyond the clip at all. Arcs wholly inside the lane return `None` and are exported as
/// one exact arc primitive.
fn clipped_arc_span(
    primitive: &rgv_alignment_scene::CpuSummaryProjectedPrimitive,
    clip: rgv_alignment_scene::CpuClipRect,
) -> Option<(f64, f64)> {
    if !matches!(
        primitive.shape,
        CpuSummaryPrimitiveShape::JunctionArcUp | CpuSummaryPrimitiveShape::JunctionArcDown
    ) {
        return None;
    }
    let right = primitive.x + primitive.width;
    let clip_right = clip.x + clip.width;
    if primitive.x >= clip.x && right <= clip_right {
        return None;
    }
    Some((primitive.x.max(clip.x), right.min(clip_right)))
}

/// Number of display rectangles [`push_clipped_arc_slices`] emits for `primitive`; zero when the
/// primitive is not an arc that leaves the lane.
fn clipped_arc_slice_count(
    primitive: &rgv_alignment_scene::CpuSummaryProjectedPrimitive,
    clip: rgv_alignment_scene::CpuClipRect,
) -> usize {
    clipped_arc_span(primitive, clip).map_or(0, |(left, right)| {
        if right > left {
            ((right - left) / CLIPPED_ARC_SLICE_WIDTH).ceil().max(1.0) as usize
        } else {
            // Nothing visible, but report one so the caller still routes the arc here instead of
            // exporting the unclipped primitive.
            1
        }
    })
}

/// Exports the visible part of a junction arc whose unclamped span leaves its lane.
///
/// The display list has no per-primitive clip, so an arc that extends beyond the lane cannot be
/// exported as one arc primitive without painting over neighbouring panels. Its visible part is
/// instead traced as thin rectangles that follow the same parabola and stroke width the arc
/// shader draws, parameterised over the full span, so the off-screen end stays off-screen.
fn push_clipped_arc_slices(
    rectangles: &mut Vec<RectPrimitive>,
    primitive: &rgv_alignment_scene::CpuSummaryProjectedPrimitive,
    clip: rgv_alignment_scene::CpuClipRect,
    color: Rgba,
) {
    let Some((left, right)) = clipped_arc_span(primitive, clip) else {
        return;
    };
    let upward = primitive.shape == CpuSummaryPrimitiveShape::JunctionArcUp;
    let stroke = f64::from(rgv_layout::junction_arc_stroke_width(
        primitive.height as f32,
    ));
    let inner_height = (primitive.height - stroke).max(0.0);
    let centre_line = |x: f64| {
        let centred = ((x - primitive.x) / primitive.width).clamp(0.0, 1.0) - 0.5;
        let offset = stroke * 0.5 + inner_height * 4.0 * centred * centred;
        if upward {
            primitive.y + offset
        } else {
            primitive.y + primitive.height - offset
        }
    };
    let mut slice_left = left;
    while slice_left < right {
        let slice_right = (slice_left + CLIPPED_ARC_SLICE_WIDTH).min(right);
        let (first, second) = (centre_line(slice_left), centre_line(slice_right));
        let top = (first.min(second) - stroke * 0.5).max(primitive.y);
        let bottom = (first.max(second) + stroke * 0.5).min(primitive.y + primitive.height);
        rectangles.push(RectPrimitive {
            x: slice_left as f32,
            y: top as f32,
            width: (slice_right - slice_left) as f32,
            height: (bottom - top).max(0.0) as f32,
            color,
            object_id: 0,
            shape: RectShape::Rectangle,
        });
        slice_left += CLIPPED_ARC_SLICE_WIDTH;
    }
}

fn rgba([red, green, blue, alpha]: [f32; 4]) -> Rgba {
    Rgba::new(red, green, blue, alpha)
}

/// CPU mirror of the colour rules in `alignment_summary.wgsl`.
fn summary_color(
    component: CpuSummaryProjectedComponent,
    options: &AlignmentDerivedLaneOptions,
    palette: &AlignmentPalette,
) -> Rgba {
    use CpuSummaryProjectedComponent as C;
    use PaletteToken as T;
    rgba(palette.color(match component {
        C::Depth { .. } => T::SUMMARY_DEPTH,
        C::CoverageCenterLine => T::COVERAGE_CENTER_LINE,
        C::CoverageSelectedPosition { .. } => T::COVERAGE_SELECTED_POSITION,
        C::Allele { allele, .. } => match allele {
            CpuAlleleToken::A => T::BASE_A,
            CpuAlleleToken::C => T::BASE_C,
            CpuAlleleToken::G => T::BASE_G,
            CpuAlleleToken::T => T::BASE_T,
            _ => T::BASE_N,
        },
        C::Bisulfite { observation, .. } => match observation {
            BisulfiteObservationToken::Protected => T::BISULFITE_PROTECTED,
            BisulfiteObservationToken::Converted => T::BISULFITE_CONVERTED,
        },
        C::JunctionBaseline { .. } => T::JUNCTION_BASELINE,
        C::Junction { strand, .. } => match strand {
            CpuJunctionStrandToken::Positive => T::JUNCTION_POSITIVE,
            CpuJunctionStrandToken::Negative => T::JUNCTION_NEGATIVE,
            CpuJunctionStrandToken::Unknown => T::JUNCTION_UNKNOWN,
        },
        C::JunctionAnnotation { status, .. } => match status {
            JunctionAnnotationStatus::Annotated => T::JUNCTION_ANNOTATED,
            JunctionAnnotationStatus::Novel => T::JUNCTION_NOVEL,
            JunctionAnnotationStatus::Unknown => T::SUMMARY_NEUTRAL,
        },
        C::Modification { series, .. } => match options {
            // Open-ended: one colour per modification code, the same under every theme.
            AlignmentDerivedLaneOptions::Modifications(options)
                if options.color_mode == ModifiedBaseColorMode::ByType =>
            {
                return modification_code_color(series.code);
            }
            _ => T::SUMMARY_MODIFICATION,
        },
        _ => T::SUMMARY_NEUTRAL,
    }))
}

#[cfg(test)]
mod tests {
    use rgv_alignment_scene::{CpuClipRect, CpuSummaryProjectedPrimitive};

    use super::*;

    const CLIP: CpuClipRect = CpuClipRect {
        x: 100.0,
        y: 0.0,
        width: 200.0,
        height: 60.0,
    };

    fn arc(x: f64, width: f64, shape: CpuSummaryPrimitiveShape) -> CpuSummaryProjectedPrimitive {
        CpuSummaryProjectedPrimitive {
            x,
            y: 10.0,
            width,
            height: 20.0,
            shape,
            component: CpuSummaryProjectedComponent::JunctionBaseline { row: 0 },
            draw_order: 0,
            text: None,
        }
    }

    #[test]
    fn arcs_inside_the_lane_are_exported_as_one_exact_primitive() {
        let inside = arc(120.0, 150.0, CpuSummaryPrimitiveShape::JunctionArcUp);
        assert_eq!(clipped_arc_slice_count(&inside, CLIP), 0);
        let touching = arc(100.0, 200.0, CpuSummaryPrimitiveShape::JunctionArcDown);
        assert_eq!(clipped_arc_slice_count(&touching, CLIP), 0);
        // Non-arc shapes are never sliced, whatever their extent.
        let bar = arc(0.0, 1_000.0, CpuSummaryPrimitiveShape::Bar);
        assert_eq!(clipped_arc_slice_count(&bar, CLIP), 0);
    }

    #[test]
    fn arcs_leaving_the_lane_are_clipped_and_keep_their_unclamped_curve() {
        // The arc starts 100 points left of the lane and ends in its middle, so the lane's left
        // edge is the apex and the stroke falls monotonically to the baseline at x = 200.
        let upward = arc(0.0, 200.0, CpuSummaryPrimitiveShape::JunctionArcUp);
        let mut slices = Vec::new();
        push_clipped_arc_slices(&mut slices, &upward, CLIP, Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(slices.len(), clipped_arc_slice_count(&upward, CLIP));
        assert_eq!(slices.len(), 200);
        for slice in &slices {
            assert!(slice.x >= 100.0 && slice.x + slice.width <= 200.0 + 1.0e-3);
            assert!(slice.y >= 10.0 - 1.0e-3 && slice.y + slice.height <= 30.0 + 1.0e-3);
            assert!(slice.height > 0.0);
            assert_eq!(slice.shape, RectShape::Rectangle);
        }
        for pair in slices.windows(2) {
            assert!((pair[0].x + pair[0].width - pair[1].x).abs() < 1.0e-3);
            assert!(pair[1].y >= pair[0].y - 1.0e-4);
        }
        let stroke = rgv_layout::junction_arc_stroke_width(20.0);
        // At the lane edge the curve is at its apex (the top of the bounds), not at the
        // baseline where a span clamped to the lane would have started it.
        assert!((slices[0].y - 10.0).abs() < 0.05);
        let last = slices.last().unwrap();
        assert!((last.y + last.height - 30.0).abs() < 1.0e-3);
        assert!(last.height >= stroke * 0.5);

        // The mirrored arc leaving on the right is clipped at the right edge.
        let downward = arc(250.0, 400.0, CpuSummaryPrimitiveShape::JunctionArcDown);
        let mut slices = Vec::new();
        push_clipped_arc_slices(&mut slices, &downward, CLIP, Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(slices.len(), clipped_arc_slice_count(&downward, CLIP));
        assert_eq!(slices.len(), 100);
        assert!((slices[0].x - 250.0).abs() < 1.0e-3);
        let last = slices.last().unwrap();
        assert!((last.x + last.width - 300.0).abs() < 1.0e-3);
        // A downward arc starts on its top edge (the baseline) and descends.
        assert!((slices[0].y - 10.0).abs() < 1.0e-3);
        assert!(last.y > slices[0].y);
    }

    /// A soft clip's zigzag is exported as one slice per half point of height, each covering
    /// exactly the columns the shader keeps at that height, inside the edge's own extent; a
    /// hard clip's bar is not sliced.
    #[test]
    fn soft_clip_sawtooth_exports_as_slices_that_trace_the_shader_zigzag() {
        use rgv_alignment_scene::{
            AlignmentBodyColorToken, AlignmentComponent, AlignmentGeometryToken, AlignmentHandle,
            AlignmentInteractionToken, AlignmentSourceRevision, AlignmentStyleToken, ClipEdgeToken,
            CpuProjectedPrimitive, RevisionFingerprint, SourceFingerprint,
        };
        let soft = ClipEdgeToken {
            at_end: true,
            hard: false,
            behind_arrow: true,
        };
        let mut edge = CpuProjectedPrimitive {
            x: 40.0,
            y: 10.0,
            width: 6.0,
            height: 11.0,
            shape: CpuPrimitiveShape::ClipEdge,
            geometry: AlignmentGeometryToken::ClipEdge(soft),
            handle: AlignmentHandle::new(
                AlignmentSourceRevision::new(
                    SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                    RevisionFingerprint::new(3, 4),
                ),
                rgv_model::AlignmentSourcePosition::bam_virtual_offset(5),
            ),
            component: AlignmentComponent::SoftClip {
                cigar_index: 1,
                read_start: 6,
                length: 4,
            },
            style: AlignmentStyleToken {
                flags: 0,
                mapping_quality: None,
                operation: None,
                body: AlignmentBodyColorToken::Neutral,
            },
            interaction: AlignmentInteractionToken::default(),
            draw_order: 0,
            text: None,
            row: 0,
            arrow: None,
        };
        assert_eq!(sawtooth_slice_count(&edge), 22);
        let mut slices = Vec::new();
        push_sawtooth_slices(&mut slices, &edge, Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(slices.len(), 22);
        let mut centres = Vec::new();
        for slice in &slices {
            let (left, right) = (f64::from(slice.x), f64::from(slice.x + slice.width));
            assert!(left >= 40.0 - 1.0e-4 && right <= 46.0 + 1.0e-4, "{slice:?}");
            assert!(right - left < 6.0, "{slice:?} fills the edge");
            let local_y = f64::from(slice.y) + f64::from(slice.height) * 0.5 - 10.0;
            // The slice's middle column is painted by the shader rule, its outside is not.
            let middle = (left + right) * 0.5 - 40.0;
            assert!(rgv_alignment_scene::clip_edge_covers(
                middle, local_y, 6.0, 11.0, soft
            ));
            if left > 40.2 {
                assert!(!rgv_alignment_scene::clip_edge_covers(
                    left - 40.0 - 0.1,
                    local_y,
                    6.0,
                    11.0,
                    soft
                ));
            }
            centres.push(middle);
        }
        let (low, high) = centres
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), centre| {
                (low.min(*centre), high.max(*centre))
            });
        assert!(high - low > 3.0, "{centres:?}");
        // Zoomed out (below one point per base) the projector draws the edge as a plain
        // hairline: exported as one rectangle, not sliced.
        edge.shape = CpuPrimitiveShape::BoundaryHairline;
        edge.width = 1.0;
        assert_eq!(sawtooth_slice_count(&edge), 0);
        edge.shape = CpuPrimitiveShape::ClipEdge;
        edge.geometry = AlignmentGeometryToken::ClipEdge(ClipEdgeToken { hard: true, ..soft });
        assert_eq!(sawtooth_slice_count(&edge), 0);
    }

    /// A primitive at a read's tip is exported as exactly the outline the GPU paints and the
    /// CPU projector picks: the read's one diagonal (min(0.55 x height, 10 bases) from the
    /// read's tip, none under half a point), whatever the primitive's own width, so a one-base
    /// terminal or base cell narrower than the head is a trimmed triangle, not a smaller arrow
    /// of its own. Unclipped it is at most a rectangle and a chevron; clipped by the pane, or a
    /// cell short of the tip, it is traced with slices.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn primitives_at_a_read_tip_export_the_shared_arrowhead_outline() {
        use rgv_alignment_scene::{
            AlignmentBodyColorToken, AlignmentComponent, AlignmentGeometryToken, AlignmentHandle,
            AlignmentInteractionToken, AlignmentSourceRevision, AlignmentStyleToken, CpuArrowhead,
            CpuProjectedPrimitive, RevisionFingerprint, SourceFingerprint,
        };
        let primitive =
            |x: f64, y: f64, width: f64, height: f64, arrow: CpuArrowhead| CpuProjectedPrimitive {
                x,
                y,
                width,
                height,
                shape: if arrow.points_right {
                    CpuPrimitiveShape::DirectionTerminalRight
                } else {
                    CpuPrimitiveShape::DirectionTerminalLeft
                },
                geometry: if arrow.points_right {
                    AlignmentGeometryToken::DirectionalTerminalRight
                } else {
                    AlignmentGeometryToken::DirectionalTerminalLeft
                },
                handle: AlignmentHandle::new(
                    AlignmentSourceRevision::new(
                        SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                        RevisionFingerprint::new(3, 4),
                    ),
                    rgv_model::AlignmentSourcePosition::bam_virtual_offset(5),
                ),
                component: AlignmentComponent::DirectionTerminal,
                style: AlignmentStyleToken {
                    flags: 0,
                    mapping_quality: None,
                    operation: None,
                    body: AlignmentBodyColorToken::Neutral,
                },
                interaction: AlignmentInteractionToken::default(),
                draw_order: 0,
                text: None,
                row: 0,
                arrow: Some(arrow),
            };
        // A read 11 points tall whose tip is at x = 100 (right) or x = 20 (left); at 2 points
        // per base the head is 0.55 x 11 = 6.05 points long.
        let right = CpuArrowhead {
            tip_x: 100.0,
            centre_y: 15.5,
            height: 11.0,
            points_per_base: 2.0,
            points_right: true,
        };
        let left = CpuArrowhead {
            tip_x: 20.0,
            points_right: false,
            ..right
        };
        let zoomed_out = |arrow: CpuArrowhead, points_per_base: f64| CpuArrowhead {
            points_per_base,
            ..arrow
        };
        // (primitive, exported exactly, most parts)
        let cases = [
            // The block: wider than the head.
            (primitive(40.0, 10.0, 60.0, 11.0, right), true, 2),
            (primitive(20.0, 10.0, 60.0, 11.0, left), true, 2),
            // The one-base terminal or base cell at 2 points per base: trimmed to a triangle.
            (primitive(98.0, 10.0, 2.0, 11.0, right), true, 1),
            (primitive(20.0, 10.0, 2.0, 11.0, left), true, 1),
            // At 4 and 10 points per base.
            (primitive(96.0, 10.0, 4.0, 11.0, right), true, 1),
            (primitive(20.0, 10.0, 10.0, 11.0, left), true, 2),
            // Clipped by the pane: the tip (3 points beyond its right edge) and the top row.
            (primitive(40.0, 12.0, 57.0, 9.0, right), false, 1 + 18),
            // Zoomed out to 0.4 points per base the head is ten bases, 4 points long ...
            (
                primitive(60.0, 10.0, 40.0, 11.0, zoomed_out(right, 0.4)),
                true,
                2,
            ),
            (
                primitive(20.0, 10.0, 1.0, 11.0, zoomed_out(left, 0.4)),
                true,
                1,
            ),
            // ... and at 0.04 points per base under half a point: no head, a plain rectangle.
            (
                primitive(60.0, 10.0, 40.0, 11.0, zoomed_out(right, 0.04)),
                true,
                1,
            ),
            (
                primitive(20.0, 10.0, 1.0, 11.0, zoomed_out(left, 0.04)),
                true,
                1,
            ),
            // A mismatch cell short of the tip (`InsideArrowhead`), its edge one base (2 points)
            // from the tip: cut by the same diagonal, traced with slices ...
            (primitive(96.0, 10.0, 2.0, 11.0, right), false, 22),
            (primitive(22.0, 10.0, 2.0, 11.0, left), false, 22),
            // ... three bases in at 0.4 points per base (a one-point cell within the 4-point
            // head) ...
            (
                primitive(97.8, 10.0, 1.0, 11.0, zoomed_out(right, 0.4)),
                false,
                22,
            ),
            // ... and four bases in, beyond the head: a whole rectangle.
            (primitive(90.0, 10.0, 2.0, 11.0, right), true, 1),
        ];
        assert!((zoomed_out(right, 0.4).length() - 4.0).abs() < 1.0e-9);
        assert!(zoomed_out(right, 0.04).length().abs() < f64::EPSILON);
        for (index, (primitive, exact, most)) in cases.into_iter().enumerate() {
            let arrow = primitive.arrow.unwrap();
            let mut parts = Vec::new();
            directional_parts(&primitive, arrow, |part| parts.push(part));
            assert_eq!(parts.len(), directional_part_count(&primitive));
            assert!(!parts.is_empty() && parts.len() <= most, "case {index}");
            let rectangles = parts
                .iter()
                .map(|part| RectPrimitive {
                    x: part.x as f32,
                    y: part.y as f32,
                    width: part.width as f32,
                    height: part.height as f32,
                    color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                    object_id: 0,
                    shape: part.shape,
                })
                .collect::<Vec<_>>();
            // Slices trace the diagonal at their middle height, so off it by at most a quarter
            // of a slice times its slope.
            let tolerance = if exact {
                1.0e-3
            } else {
                ARROW_SLICE_HEIGHT * 0.5 * arrow.length() / (arrow.height * 0.5) + 1.0e-3
            };
            for column in 0..400 {
                let x = primitive.x - 1.0 + f64::from(column) * (primitive.width + 2.0) / 400.0;
                for row in 0..120 {
                    let y = primitive.y - 1.0 + f64::from(row) * (primitive.height + 2.0) / 120.0;
                    let near_outline = arrow.outline_distance_at(y).is_some_and(|outline| {
                        (outline - arrow.distance_from_tip(x)).abs() < tolerance
                    });
                    let near_edge = [primitive.x, primitive.x + primitive.width]
                        .iter()
                        .any(|edge| (x - edge).abs() < 1.0e-3)
                        || [primitive.y, primitive.y + primitive.height]
                            .iter()
                            .any(|edge| (y - edge).abs() < 1.0e-3);
                    if near_outline || near_edge {
                        continue;
                    }
                    // The rasteriser visits the rectangle's own pixels; its shape rule decides.
                    let (px, py) = (x as f32, y as f32);
                    let exported = rectangles.iter().any(|rectangle| {
                        px >= rectangle.x
                            && px < rectangle.x + rectangle.width
                            && py >= rectangle.y
                            && py < rectangle.y + rectangle.height
                            && crate::rectangle_contains_point(*rectangle, px, py)
                    });
                    assert_eq!(
                        exported,
                        primitive.contains(x, y),
                        "case {index} at ({x}, {y}): {rectangles:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn arcs_wholly_outside_the_lane_export_nothing() {
        let outside = arc(400.0, 50.0, CpuSummaryPrimitiveShape::JunctionArcUp);
        assert_eq!(clipped_arc_slice_count(&outside, CLIP), 1);
        let mut slices = Vec::new();
        push_clipped_arc_slices(&mut slices, &outside, CLIP, Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert!(slices.is_empty());
    }
}
