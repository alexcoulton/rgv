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
/// those frames pins the source/reference/style authority for the job lifetime.
#[allow(clippy::too_many_lines)]
pub fn project_semantic_alignment_export<S: std::hash::BuildHasher>(
    panels: &[AlignmentPanelSceneFrame],
    ledger: &rgv_alignment_scene::AllocationLedger,
    interactions: &HashMap<
        (rgv_core::PanelId, rgv_core::TrackId),
        rgv_alignment_scene::AlignmentInteractionState,
        S,
    >,
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
                    output.reserve(
                        ledger,
                        projected
                            .primitives
                            .len()
                            .saturating_mul(if modification_background { 2 } else { 1 }),
                    )?;
                    if modification_background {
                        for primitive in projected.primitives.iter() {
                            output.display.rectangles.push(RectPrimitive {
                                x: primitive.x as f32,
                                y: camera.clip().y as f32,
                                width: primitive.width as f32,
                                height: camera.clip().height as f32,
                                color: Rgba::new(0.14, 0.44, 0.82, 0.20),
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
                        output.display.rectangles.push(RectPrimitive {
                            x: primitive.x as f32,
                            y: primitive.y as f32,
                            width: primitive.width as f32,
                            height: primitive.height as f32,
                            color: summary_color(primitive.component, &derived.options),
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
                    let color = crate::alignment_scene::semantic_alignment_color(
                        primitive.component,
                        primitive.style,
                    );
                    let [r, g, b, a] = crate::alignment_scene::reference_interaction_color(
                        color,
                        primitive.interaction,
                    );
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

// Reference palette for alignment_summary.wgsl, kept alongside renderer code.
fn summary_color(
    component: CpuSummaryProjectedComponent,
    options: &AlignmentDerivedLaneOptions,
) -> Rgba {
    use CpuSummaryProjectedComponent as C;
    match component {
        C::Depth { .. } => Rgba::new(0.48, 0.48, 0.48, 0.90),
        C::CoverageCenterLine => Rgba::new(0.72, 0.75, 0.82, 0.72),
        C::CoverageSelectedPosition { .. } => Rgba::new(1.0, 0.78, 0.18, 0.92),
        C::Allele { allele, .. } => match allele {
            CpuAlleleToken::A => Rgba::new(0.24, 0.78, 0.38, 1.0),
            CpuAlleleToken::C => Rgba::new(0.22, 0.52, 0.96, 1.0),
            CpuAlleleToken::G => Rgba::new(0.96, 0.65, 0.18, 1.0),
            CpuAlleleToken::T => Rgba::new(0.94, 0.28, 0.28, 1.0),
            _ => Rgba::new(0.60, 0.64, 0.70, 1.0),
        },
        C::Bisulfite { observation, .. } => match observation {
            BisulfiteObservationToken::Protected => Rgba::new(0.86, 0.46, 0.12, 1.0),
            BisulfiteObservationToken::Converted => Rgba::new(0.18, 0.62, 0.78, 1.0),
        },
        C::JunctionBaseline { .. } => Rgba::new(0.36, 0.38, 0.42, 0.24),
        C::Junction { strand, .. } => match strand {
            CpuJunctionStrandToken::Positive => Rgba::new(0.84, 0.30, 0.38, 0.82),
            CpuJunctionStrandToken::Negative => Rgba::new(0.25, 0.45, 0.82, 0.82),
            CpuJunctionStrandToken::Unknown => Rgba::new(0.42, 0.45, 0.50, 0.76),
        },
        C::JunctionAnnotation { status, .. } => match status {
            JunctionAnnotationStatus::Annotated => Rgba::new(0.16, 0.58, 0.28, 1.0),
            JunctionAnnotationStatus::Novel => Rgba::new(0.92, 0.48, 0.10, 1.0),
            JunctionAnnotationStatus::Unknown => Rgba::new(0.46, 0.48, 0.52, 0.92),
        },
        C::Modification { series, .. } => match options {
            AlignmentDerivedLaneOptions::Modifications(options)
                if options.color_mode == ModifiedBaseColorMode::ByType =>
            {
                modification_code_color(series.code)
            }
            _ => Rgba::new(0.90, 0.24, 0.92, 0.92),
        },
        _ => Rgba::new(0.46, 0.48, 0.52, 0.92),
    }
}
