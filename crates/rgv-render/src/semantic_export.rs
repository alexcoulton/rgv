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
                output.reserve(ledger, frame.primitives.len())?;
                for primitive in frame
                    .primitives
                    .iter()
                    .filter(|p| p.shape != CpuPrimitiveShape::TextLabel)
                {
                    let color = palette.resolve(crate::alignment_scene::semantic_alignment_color(
                        primitive.component,
                        primitive.style,
                    ));
                    let [r, g, b, a] = palette.interaction_color(color, primitive.interaction);
                    output.display.rectangles.push(RectPrimitive {
                        x: primitive.x as f32,
                        y: primitive.y as f32,
                        width: primitive.width as f32,
                        height: primitive.height as f32,
                        color: Rgba::new(r, g, b, a),
                        object_id: 0,
                        shape: match primitive.shape {
                            CpuPrimitiveShape::DirectionTerminalLeft => {
                                RectShape::DirectionalTerminalLeft
                            }
                            CpuPrimitiveShape::DirectionTerminalRight => {
                                RectShape::DirectionalTerminalRight
                            }
                            _ => RectShape::Rectangle,
                        },
                    });
                }
            }
        }
    }
    Ok(output)
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

    #[test]
    fn arcs_wholly_outside_the_lane_export_nothing() {
        let outside = arc(400.0, 50.0, CpuSummaryPrimitiveShape::JunctionArcUp);
        assert_eq!(clipped_arc_slice_count(&outside, CLIP), 1);
        let mut slices = Vec::new();
        push_clipped_arc_slices(&mut slices, &outside, CLIP, Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert!(slices.is_empty());
    }
}
