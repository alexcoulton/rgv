use std::{collections::HashSet, mem::size_of, sync::Arc};

use rgv_core::{CoverageScaleTransform, GenomicInterval, JunctionDisplayMode, SashimiCountMode};
use rgv_hts::{
    JunctionEvidence, ModificationBin, ModificationCode, ModificationSeriesKey, PairSummary,
};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentSceneSnapshot, AnnotatedJunctionSemantic, CompleteCoverage,
    CpuClipRect, JunctionAnnotationStatus, LedgerError, ReservationKind, SceneRevision,
    SummaryPageFamily, SummarySemanticComponent, TransientResourceClass,
    TransientResourceReservation,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuSummaryCamera {
    interval: GenomicInterval,
    clip: CpuClipRect,
}

impl CpuSummaryCamera {
    pub fn new(
        interval: GenomicInterval,
        clip: CpuClipRect,
    ) -> Result<Self, CpuSummaryProjectionError> {
        if !clip.x.is_finite()
            || !clip.y.is_finite()
            || !clip.width.is_finite()
            || clip.width <= 0.0
            || !clip.height.is_finite()
            || clip.height <= 0.0
        {
            return Err(CpuSummaryProjectionError::InvalidCamera);
        }
        Ok(Self { interval, clip })
    }

    #[must_use]
    pub const fn interval(self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub const fn clip(self) -> CpuClipRect {
        self.clip
    }

    fn x(self, position: u64) -> f64 {
        self.clip.x
            + position.saturating_sub(self.interval.start()) as f64 * self.clip.width
                / self.interval.len() as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuDepthProjectionOptions {
    pub scale_maximum: Option<u32>,
    pub transform: CoverageScaleTransform,
    pub show_center_line: bool,
}

impl Default for CpuDepthProjectionOptions {
    fn default() -> Self {
        Self {
            scale_maximum: None,
            transform: CoverageScaleTransform::Linear,
            show_center_line: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuAlleleProjectionOptions {
    pub minimum_fraction: f64,
    pub quality_weighted: bool,
}

impl Default for CpuAlleleProjectionOptions {
    fn default() -> Self {
        Self {
            minimum_fraction: 0.2,
            quality_weighted: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuJunctionProjectionOptions {
    pub minimum_depth: u32,
    pub minimum_flank: u32,
    pub display_mode: JunctionDisplayMode,
    pub scale_maximum: Option<f64>,
    pub count_mode: SashimiCountMode,
    pub show_labels: bool,
}

impl Default for CpuJunctionProjectionOptions {
    fn default() -> Self {
        Self {
            minimum_depth: 1,
            minimum_flank: 0,
            display_mode: JunctionDisplayMode::Collapsed,
            scale_maximum: None,
            count_mode: SashimiCountMode::Raw,
            show_labels: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuModificationProjectionOptions {
    pub probability_weighted: bool,
    pub hidden_codes: Vec<ModificationCode>,
}

impl Default for CpuModificationProjectionOptions {
    fn default() -> Self {
        Self {
            probability_weighted: true,
            hidden_codes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CpuSummaryProjectionOptions {
    Depth(CpuDepthProjectionOptions),
    Alleles(CpuAlleleProjectionOptions),
    Junctions(CpuJunctionProjectionOptions),
    Modifications(CpuModificationProjectionOptions),
    Pairs,
    FilterProvenance,
}

impl CpuSummaryProjectionOptions {
    #[must_use]
    pub const fn family(&self) -> SummaryPageFamily {
        match self {
            Self::Depth(_) => SummaryPageFamily::Depth,
            Self::Alleles(_) => SummaryPageFamily::Alleles,
            Self::Junctions(_) => SummaryPageFamily::Junctions,
            Self::Modifications(_) => SummaryPageFamily::Modifications,
            Self::Pairs => SummaryPageFamily::Pairs,
            Self::FilterProvenance => SummaryPageFamily::FilterProvenance,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuSummaryPrimitiveShape {
    Bar,
    CenterLine,
    JunctionArcUp,
    JunctionArcDown,
    JunctionBaseline,
    PointMarker,
    TextLabel,
    MetadataBand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuAlleleToken {
    A,
    C,
    G,
    T,
    N,
    Deletion,
    Insertion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuJunctionStrandToken {
    Positive,
    Negative,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuSummaryProjectedComponent {
    Depth {
        depth: u32,
        forward_depth: u32,
        reverse_depth: u32,
    },
    CoverageCenterLine,
    Allele {
        allele: CpuAlleleToken,
        observations: u64,
        fraction_bits: u64,
    },
    JunctionBaseline {
        row: u32,
    },
    Junction {
        evidence: JunctionEvidence,
        annotation: Option<JunctionAnnotationStatus>,
        strand: CpuJunctionStrandToken,
        value_bits: u64,
        row: u32,
    },
    JunctionLabel {
        evidence: JunctionEvidence,
        annotation: Option<JunctionAnnotationStatus>,
    },
    JunctionAnnotation {
        evidence: JunctionEvidence,
        status: JunctionAnnotationStatus,
    },
    Modification {
        series: ModificationSeriesKey,
        bin: ModificationBin,
        fraction_bits: u64,
    },
    PairCandidate {
        handle: AlignmentHandle,
        score: u64,
        template_length: u64,
    },
    PairStatistics(PairSummary),
    FilterProvenance(CompleteCoverage),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuSummaryProjectedPrimitive {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub shape: CpuSummaryPrimitiveShape,
    pub component: CpuSummaryProjectedComponent,
    pub draw_order: u32,
    pub text: Option<Arc<str>>,
}

#[derive(Clone, Debug)]
pub struct CpuSummaryFrame {
    pub scene_revision: SceneRevision,
    pub camera_sequence: u64,
    pub family: SummaryPageFamily,
    pub requested_interval: GenomicInterval,
    pub covered_interval: Option<GenomicInterval>,
    pub scale_maximum: Option<f64>,
    pub normalization_available: bool,
    pub primitives: Arc<Vec<CpuSummaryProjectedPrimitive>>,
    _resources: Option<Arc<CpuSummaryFrameResources>>,
}

impl PartialEq for CpuSummaryFrame {
    fn eq(&self, other: &Self) -> bool {
        self.scene_revision == other.scene_revision
            && self.camera_sequence == other.camera_sequence
            && self.family == other.family
            && self.requested_interval == other.requested_interval
            && self.covered_interval == other.covered_interval
            && self.scale_maximum == other.scale_maximum
            && self.normalization_available == other.normalization_available
            && self.primitives == other.primitives
    }
}

#[derive(Debug)]
struct CpuSummaryFrameResources {
    _projection: TransientResourceReservation,
}

#[derive(Debug, Default)]
pub struct CpuSummaryProjector;

impl CpuSummaryProjector {
    pub fn project(
        scene: &Arc<AlignmentSceneSnapshot>,
        camera_sequence: u64,
        camera: CpuSummaryCamera,
        options: &CpuSummaryProjectionOptions,
    ) -> Result<CpuSummaryFrame, CpuSummaryProjectionError> {
        let covered = intersect(camera.interval, scene.prepared_interval());
        let family = options.family();
        let maximum_facts = covered.map_or(0, |interval| {
            scene
                .summary_pages()
                .iter()
                .filter(|page| page.key().family() == family && page.interval().overlaps(interval))
                .fold(0_usize, |total, page| {
                    total.saturating_add(page.primitives().len())
                })
        });
        let maximum_output = maximum_summary_output(maximum_facts, options);
        let mut projection_resources = scene.metadata_allocation().ledger().reserve_transient(
            TransientResourceClass::CpuSummaryProjection,
            cpu_summary_projection_scratch_bytes(maximum_facts, maximum_output),
            ReservationKind::Normal,
        )?;
        let mut frame = CpuSummaryFrame {
            scene_revision: scene.revision(),
            camera_sequence,
            family,
            requested_interval: camera.interval,
            covered_interval: covered,
            scale_maximum: None,
            normalization_available: true,
            primitives: Arc::new(Vec::with_capacity(maximum_output)),
            _resources: None,
        };
        if let Some(covered) = covered {
            match options {
                CpuSummaryProjectionOptions::Depth(options) => {
                    project_depth(scene, camera, covered, *options, &mut frame);
                }
                CpuSummaryProjectionOptions::Alleles(options) => {
                    validate_fraction(options.minimum_fraction)?;
                    project_alleles(scene, camera, covered, *options, &mut frame);
                }
                CpuSummaryProjectionOptions::Junctions(options) => {
                    project_junctions(scene, camera, covered, *options, &mut frame)?;
                }
                CpuSummaryProjectionOptions::Modifications(options) => {
                    project_modifications(scene, camera, covered, options, &mut frame);
                }
                CpuSummaryProjectionOptions::Pairs => {
                    project_pairs(scene, camera, covered, &mut frame);
                }
                CpuSummaryProjectionOptions::FilterProvenance => {
                    project_filters(scene, camera, covered, &mut frame);
                }
            }
        }
        let primitives = Arc::get_mut(&mut frame.primitives)
            .expect("summary projection owns its mutable primitive vector");
        reconcile_summary_labels(primitives, camera.clip);
        primitives.sort_by(|left, right| {
            left.y
                .total_cmp(&right.y)
                .then_with(|| left.x.total_cmp(&right.x))
                .then_with(|| left.draw_order.cmp(&right.draw_order))
        });
        primitives.shrink_to_fit();
        let retained_bytes = cpu_summary_retained_bytes(primitives.capacity());
        projection_resources.shrink_to(retained_bytes)?;
        frame._resources = Some(Arc::new(CpuSummaryFrameResources {
            _projection: projection_resources,
        }));
        Ok(frame)
    }
}

fn project_depth(
    scene: &AlignmentSceneSnapshot,
    camera: CpuSummaryCamera,
    covered: GenomicInterval,
    options: CpuDepthProjectionOptions,
    frame: &mut CpuSummaryFrame,
) {
    let facts = summary_facts(scene, SummaryPageFamily::Depth, covered);
    let automatic = facts
        .iter()
        .filter_map(|(_, _, component)| match component {
            SummarySemanticComponent::Depth { depth, .. } => Some(*depth),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    let maximum = options.scale_maximum.unwrap_or(automatic).max(1);
    frame.scale_maximum = Some(f64::from(maximum));
    for (start, end, component) in facts {
        let SummarySemanticComponent::Depth {
            depth,
            forward_depth,
            reverse_depth,
            ..
        } = component
        else {
            continue;
        };
        let fraction = (f64::from(depth) / f64::from(maximum)).min(1.0);
        let fraction = match options.transform {
            CoverageScaleTransform::Linear => fraction,
            CoverageScaleTransform::SquareRoot => fraction.sqrt(),
        };
        push_bar(
            frame,
            camera,
            start,
            end,
            fraction,
            CpuSummaryProjectedComponent::Depth {
                depth,
                forward_depth,
                reverse_depth,
            },
        );
    }
    if options.show_center_line {
        push(
            frame,
            camera.clip.x + camera.clip.width * 0.5,
            camera.clip.y,
            1.0,
            camera.clip.height,
            CpuSummaryPrimitiveShape::CenterLine,
            CpuSummaryProjectedComponent::CoverageCenterLine,
            None,
        );
    }
}

fn project_alleles(
    scene: &AlignmentSceneSnapshot,
    camera: CpuSummaryCamera,
    covered: GenomicInterval,
    options: CpuAlleleProjectionOptions,
    frame: &mut CpuSummaryFrame,
) {
    for (start, end, component) in summary_facts(scene, SummaryPageFamily::Alleles, covered) {
        let SummarySemanticComponent::Alleles(bin) = component else {
            continue;
        };
        let raw_denominator = u64::from(bin.alleles.reference_observations());
        let weighted_denominator = bin.quality_weighted.total();
        let denominator = if options.quality_weighted && weighted_denominator > 0 {
            weighted_denominator
        } else {
            raw_denominator
        };
        if denominator == 0 {
            continue;
        }
        let values = [
            (CpuAlleleToken::A, b'A'),
            (CpuAlleleToken::C, b'C'),
            (CpuAlleleToken::G, b'G'),
            (CpuAlleleToken::T, b'T'),
            (CpuAlleleToken::N, b'N'),
        ];
        let mut cursor = camera.clip.y + camera.clip.height;
        for (allele, base) in values {
            let observations = if options.quality_weighted && weighted_denominator > 0 {
                bin.quality_weighted.for_base(base)
            } else {
                u64::from(bin.alleles.for_base(base))
            };
            cursor = push_allele_segment(
                frame,
                camera,
                start,
                end,
                allele,
                observations,
                denominator,
                options.minimum_fraction,
                cursor,
            );
        }
        for (allele, observations) in [
            (CpuAlleleToken::Deletion, u64::from(bin.alleles.deletion)),
            (CpuAlleleToken::Insertion, u64::from(bin.alleles.insertion)),
        ] {
            cursor = push_allele_segment(
                frame,
                camera,
                start,
                end,
                allele,
                observations,
                raw_denominator.max(1),
                options.minimum_fraction,
                cursor,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_allele_segment(
    frame: &mut CpuSummaryFrame,
    camera: CpuSummaryCamera,
    start: u64,
    end: u64,
    allele: CpuAlleleToken,
    observations: u64,
    denominator: u64,
    minimum_fraction: f64,
    cursor: f64,
) -> f64 {
    let fraction = observations as f64 / denominator as f64;
    if observations == 0 || fraction < minimum_fraction {
        return cursor;
    }
    let height = (fraction * camera.clip.height).max(1.0);
    let y = (cursor - height).max(camera.clip.y);
    let (x, width) = horizontal(camera, start, end);
    push(
        frame,
        x,
        y,
        width,
        (cursor - y).max(1.0),
        CpuSummaryPrimitiveShape::Bar,
        CpuSummaryProjectedComponent::Allele {
            allele,
            observations,
            fraction_bits: fraction.to_bits(),
        },
        None,
    );
    y
}

fn project_junctions(
    scene: &AlignmentSceneSnapshot,
    camera: CpuSummaryCamera,
    covered: GenomicInterval,
    options: CpuJunctionProjectionOptions,
    frame: &mut CpuSummaryFrame,
) -> Result<(), CpuSummaryProjectionError> {
    if options
        .scale_maximum
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        return Err(CpuSummaryProjectionError::InvalidScale);
    }
    let mut seen = HashSet::<(u64, u64)>::new();
    let mut junctions = summary_facts(scene, SummaryPageFamily::Junctions, covered)
        .into_iter()
        .filter_map(|(_, _, component)| match component {
            SummarySemanticComponent::Junction(junction)
                if seen.insert((junction.evidence.start, junction.evidence.end))
                    && junction.evidence.depth >= options.minimum_depth
                    && junction.evidence.minimum_left_flank >= options.minimum_flank
                    && junction.evidence.minimum_right_flank >= options.minimum_flank =>
            {
                Some(junction)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    junctions.sort_unstable_by_key(|junction| (junction.evidence.start, junction.evidence.end));
    let normalization = scene
        .summaries()
        .junction_tiles()
        .iter()
        .find_map(|tile| tile.summary().provenance.normalization_mapped_records);
    frame.normalization_available =
        options.count_mode == SashimiCountMode::Raw || normalization.is_some_and(|total| total > 0);
    let value = |junction: &AnnotatedJunctionSemantic| {
        junction_value(junction.evidence, options.count_mode, normalization)
    };
    let automatic = junctions.iter().map(value).fold(0.0_f64, f64::max);
    let maximum = options.scale_maximum.unwrap_or(automatic.max(f64::EPSILON));
    frame.scale_maximum = Some(maximum);
    let rows = if options.display_mode == JunctionDisplayMode::Expanded {
        expanded_junction_rows(&junctions)
    } else {
        vec![0; junctions.len()]
    };
    let row_count = rows.iter().copied().max().unwrap_or(0).saturating_add(1);
    let band_height = camera.clip.height / row_count.max(1) as f64;
    for row in 0..row_count {
        push(
            frame,
            camera.clip.x,
            camera.clip.y + (f64::from(row) + 0.5) * band_height,
            camera.clip.width,
            1.0,
            CpuSummaryPrimitiveShape::JunctionBaseline,
            CpuSummaryProjectedComponent::JunctionBaseline { row },
            None,
        );
    }
    for (annotated, row) in junctions.into_iter().zip(rows) {
        let junction = annotated.evidence;
        let (x, width) = horizontal(camera, junction.start, junction.end);
        if width <= 0.0 {
            continue;
        }
        let baseline = camera.clip.y + (f64::from(row) + 0.5) * band_height;
        for (depth, strand, up) in [
            (
                junction.positive_depth,
                CpuJunctionStrandToken::Positive,
                true,
            ),
            (
                junction.negative_depth,
                CpuJunctionStrandToken::Negative,
                false,
            ),
            (
                junction.unknown_depth,
                CpuJunctionStrandToken::Unknown,
                true,
            ),
        ] {
            if depth == 0 {
                continue;
            }
            let component_value = junction_value(junction, options.count_mode, normalization)
                * f64::from(depth)
                / f64::from(junction.depth);
            let fraction = (component_value / maximum).clamp(0.0, 1.0).sqrt();
            let height = (band_height * 0.45 * (0.18 + 0.82 * fraction)).max(3.0);
            push(
                frame,
                x,
                if up { baseline - height } else { baseline },
                width,
                height,
                if up {
                    CpuSummaryPrimitiveShape::JunctionArcUp
                } else {
                    CpuSummaryPrimitiveShape::JunctionArcDown
                },
                CpuSummaryProjectedComponent::Junction {
                    evidence: junction,
                    annotation: annotated.annotation,
                    strand,
                    value_bits: component_value.to_bits(),
                    row,
                },
                None,
            );
        }
        if let Some(status) = annotated.annotation {
            push(
                frame,
                x + width * 0.5 - 2.5,
                baseline - 2.5,
                5.0,
                5.0,
                CpuSummaryPrimitiveShape::PointMarker,
                CpuSummaryProjectedComponent::JunctionAnnotation {
                    evidence: junction,
                    status,
                },
                None,
            );
        }
        if options.show_labels {
            let text: Arc<str> =
                Arc::from(junction_label(junction, options.count_mode, normalization));
            push(
                frame,
                x + width * 0.5,
                baseline - 6.0,
                1.0,
                12.0,
                CpuSummaryPrimitiveShape::TextLabel,
                CpuSummaryProjectedComponent::JunctionLabel {
                    evidence: junction,
                    annotation: annotated.annotation,
                },
                Some(text),
            );
        }
    }
    Ok(())
}

fn reconcile_summary_labels(primitives: &mut Vec<CpuSummaryProjectedPrimitive>, clip: CpuClipRect) {
    const MIN_TEXT_HEIGHT: f64 = 5.0;
    const HORIZONTAL_PADDING: f64 = 2.0;
    const COLLISION_GAP: f64 = 2.0;
    const GLYPH_ASPECT_RATIO: f64 = 0.56;

    let clip_right = clip.x + clip.width;
    let mut retained = Vec::with_capacity(primitives.len());
    let mut candidates = Vec::new();
    for mut primitive in primitives.drain(..) {
        if primitive.shape != CpuSummaryPrimitiveShape::TextLabel {
            retained.push(primitive);
            continue;
        }
        let Some(text) = primitive.text.as_deref() else {
            continue;
        };
        if primitive.height < MIN_TEXT_HEIGHT {
            continue;
        }
        let glyph_count = text.chars().count().max(1) as f64;
        let text_width =
            glyph_count * primitive.height * GLYPH_ASPECT_RATIO + HORIZONTAL_PADDING * 2.0;
        if text_width > clip.width {
            continue;
        }
        let anchor = primitive.x;
        primitive.x = (anchor - text_width * 0.5).clamp(clip.x, clip_right - text_width);
        primitive.width = text_width;
        candidates.push(primitive);
    }
    candidates.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y))
            .then_with(|| left.draw_order.cmp(&right.draw_order))
    });

    // Accepted labels form a screen-space sweep set. Its size is constrained by non-overlapping
    // screen area rather than by an arbitrary junction or record count.
    let mut active = Vec::<CpuSummaryProjectedPrimitive>::new();
    for candidate in candidates {
        active.retain(|accepted| accepted.x + accepted.width + COLLISION_GAP > candidate.x);
        let collides = active.iter().any(|accepted| {
            accepted.y < candidate.y + candidate.height + COLLISION_GAP
                && candidate.y < accepted.y + accepted.height + COLLISION_GAP
        });
        if !collides {
            active.push(candidate.clone());
            retained.push(candidate);
        }
    }
    *primitives = retained;
}

fn project_modifications(
    scene: &AlignmentSceneSnapshot,
    camera: CpuSummaryCamera,
    covered: GenomicInterval,
    options: &CpuModificationProjectionOptions,
    frame: &mut CpuSummaryFrame,
) {
    for (start, end, component) in summary_facts(scene, SummaryPageFamily::Modifications, covered) {
        let SummarySemanticComponent::Modification { series, bin } = component else {
            continue;
        };
        if options.hidden_codes.contains(&series.code) {
            continue;
        }
        let fraction = if options.probability_weighted {
            bin.weighted_fraction()
        } else {
            bin.raw_fraction()
        };
        let Some(fraction) = fraction else {
            continue;
        };
        push_bar(
            frame,
            camera,
            start,
            end,
            fraction.clamp(0.0, 1.0),
            CpuSummaryProjectedComponent::Modification {
                series,
                bin,
                fraction_bits: fraction.to_bits(),
            },
        );
    }
    frame.scale_maximum = Some(1.0);
}

fn project_pairs(
    scene: &AlignmentSceneSnapshot,
    camera: CpuSummaryCamera,
    covered: GenomicInterval,
    frame: &mut CpuSummaryFrame,
) {
    for (start, end, component) in summary_facts(scene, SummaryPageFamily::Pairs, covered) {
        let projected = match component {
            SummarySemanticComponent::PairCandidate {
                handle,
                score,
                template_length,
            } => CpuSummaryProjectedComponent::PairCandidate {
                handle,
                score,
                template_length,
            },
            SummarySemanticComponent::PairStatistics(summary) => {
                CpuSummaryProjectedComponent::PairStatistics(summary)
            }
            _ => continue,
        };
        let (x, width) = horizontal(camera, start, end);
        push(
            frame,
            x,
            camera.clip.y,
            width.max(1.0),
            camera.clip.height,
            CpuSummaryPrimitiveShape::PointMarker,
            projected,
            None,
        );
    }
}

fn project_filters(
    scene: &AlignmentSceneSnapshot,
    camera: CpuSummaryCamera,
    covered: GenomicInterval,
    frame: &mut CpuSummaryFrame,
) {
    for (start, end, component) in
        summary_facts(scene, SummaryPageFamily::FilterProvenance, covered)
    {
        let SummarySemanticComponent::FilterProvenance(provenance) = component else {
            continue;
        };
        let (x, width) = horizontal(camera, start, end);
        push(
            frame,
            x,
            camera.clip.y,
            width,
            camera.clip.height,
            CpuSummaryPrimitiveShape::MetadataBand,
            CpuSummaryProjectedComponent::FilterProvenance(provenance),
            None,
        );
    }
}

fn summary_facts(
    scene: &AlignmentSceneSnapshot,
    family: SummaryPageFamily,
    interval: GenomicInterval,
) -> Vec<(u64, u64, SummarySemanticComponent)> {
    scene
        .summary_pages_for(family, interval)
        .into_iter()
        .flat_map(|page| {
            page.primitives().iter().filter_map(move |primitive| {
                let start = page
                    .interval()
                    .start()
                    .saturating_add(primitive.start_offset)
                    .max(interval.start());
                let end = page
                    .interval()
                    .start()
                    .saturating_add(primitive.end_offset)
                    .min(interval.end());
                (start < end).then_some((start, end, primitive.component))
            })
        })
        .collect()
}

fn push_bar(
    frame: &mut CpuSummaryFrame,
    camera: CpuSummaryCamera,
    start: u64,
    end: u64,
    fraction: f64,
    component: CpuSummaryProjectedComponent,
) {
    let (x, width) = horizontal(camera, start, end);
    let height = (fraction * camera.clip.height).max(1.0);
    push(
        frame,
        x,
        camera.clip.y + camera.clip.height - height,
        width,
        height,
        CpuSummaryPrimitiveShape::Bar,
        component,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn push(
    frame: &mut CpuSummaryFrame,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    shape: CpuSummaryPrimitiveShape,
    component: CpuSummaryProjectedComponent,
    text: Option<Arc<str>>,
) {
    let primitives = Arc::get_mut(&mut frame.primitives)
        .expect("summary projection owns its mutable primitive vector");
    let draw_order = u32::try_from(primitives.len()).unwrap_or(u32::MAX);
    primitives.push(CpuSummaryProjectedPrimitive {
        x,
        y,
        width: width.max(1.0),
        height: height.max(1.0),
        shape,
        component,
        draw_order,
        text,
    });
}

fn maximum_summary_output(facts: usize, options: &CpuSummaryProjectionOptions) -> usize {
    match options {
        CpuSummaryProjectionOptions::Depth(_) => facts.saturating_add(1),
        CpuSummaryProjectionOptions::Alleles(_) => facts.saturating_mul(7),
        CpuSummaryProjectionOptions::Junctions(_) => facts.saturating_mul(6),
        CpuSummaryProjectionOptions::Modifications(_)
        | CpuSummaryProjectionOptions::Pairs
        | CpuSummaryProjectionOptions::FilterProvenance => facts,
    }
}

fn cpu_summary_projection_scratch_bytes(facts: usize, output: usize) -> usize {
    let fact_bytes = facts
        .saturating_mul(size_of::<(u64, u64, SummarySemanticComponent)>())
        .saturating_mul(4);
    let output_bytes = output
        .saturating_mul(size_of::<CpuSummaryProjectedPrimitive>())
        .saturating_mul(3);
    let hash_entries = facts
        .saturating_mul(2)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX);
    let hash_bytes = hash_entries.saturating_mul(
        size_of::<(u64, u64)>()
            .saturating_add(size_of::<usize>())
            .saturating_add(1),
    );
    fact_bytes
        .saturating_add(output_bytes)
        .saturating_add(hash_bytes)
        .saturating_add(cpu_summary_retained_bytes(output))
}

fn cpu_summary_retained_bytes(primitives: usize) -> usize {
    primitives
        .saturating_mul(size_of::<CpuSummaryProjectedPrimitive>())
        .saturating_add(size_of::<Vec<CpuSummaryProjectedPrimitive>>())
        .saturating_add(size_of::<usize>().saturating_mul(2))
}

fn horizontal(camera: CpuSummaryCamera, start: u64, end: u64) -> (f64, f64) {
    let start = start.max(camera.interval.start());
    let end = end.min(camera.interval.end());
    let x = camera.x(start);
    (x, (camera.x(end) - x).max(1.0))
}

fn expanded_junction_rows(junctions: &[AnnotatedJunctionSemantic]) -> Vec<u32> {
    let mut ends = Vec::<u64>::new();
    junctions
        .iter()
        .map(|junction| {
            if let Some((row, end)) = ends
                .iter_mut()
                .enumerate()
                .find(|(_, end)| **end <= junction.evidence.start)
            {
                *end = junction.evidence.end;
                u32::try_from(row).unwrap_or(u32::MAX)
            } else {
                ends.push(junction.evidence.end);
                u32::try_from(ends.len() - 1).unwrap_or(u32::MAX)
            }
        })
        .collect()
}

fn junction_value(
    junction: JunctionEvidence,
    mode: SashimiCountMode,
    normalization: Option<u64>,
) -> f64 {
    match mode {
        SashimiCountMode::Raw => f64::from(junction.depth),
        SashimiCountMode::CountsPerMillion => normalization.filter(|total| *total > 0).map_or_else(
            || f64::from(junction.depth),
            |total| f64::from(junction.depth) * 1_000_000.0 / total as f64,
        ),
    }
}

fn junction_label(
    junction: JunctionEvidence,
    mode: SashimiCountMode,
    normalization: Option<u64>,
) -> String {
    match (mode, normalization) {
        (SashimiCountMode::CountsPerMillion, Some(total)) if total > 0 => {
            let value = junction_value(junction, mode, normalization);
            if value < 10.0 {
                format!("{value:.1} CPM")
            } else {
                format!("{value:.0} CPM")
            }
        }
        (SashimiCountMode::CountsPerMillion, _) => format!("{} raw", junction.depth),
        (SashimiCountMode::Raw, _) => junction.depth.to_string(),
    }
}

fn validate_fraction(value: f64) -> Result<(), CpuSummaryProjectionError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(CpuSummaryProjectionError::InvalidAlleleFraction)
    }
}

fn intersect(left: GenomicInterval, right: GenomicInterval) -> Option<GenomicInterval> {
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

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum CpuSummaryProjectionError {
    #[error("summary CPU camera geometry is invalid")]
    InvalidCamera,
    #[error("summary scale maximum must be finite and positive")]
    InvalidScale,
    #[error("minimum allele fraction must be within zero and one")]
    InvalidAlleleFraction,
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn junction_label(
        start: u64,
        x: f64,
        text: &'static str,
        draw_order: u32,
    ) -> CpuSummaryProjectedPrimitive {
        CpuSummaryProjectedPrimitive {
            x,
            y: 10.0,
            width: 1.0,
            height: 12.0,
            shape: CpuSummaryPrimitiveShape::TextLabel,
            component: CpuSummaryProjectedComponent::JunctionLabel {
                evidence: JunctionEvidence {
                    start,
                    end: start + 20,
                    depth: 10,
                    ..JunctionEvidence::default()
                },
                annotation: None,
            },
            draw_order,
            text: Some(Arc::from(text)),
        }
    }

    #[test]
    fn junction_label_collision_is_determined_by_screen_geometry_without_a_count_cap() {
        let clip = CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 40.0,
        };
        let mut overlapping = vec![
            junction_label(10, 50.0, "10", 0),
            junction_label(20, 55.0, "20", 1),
        ];
        reconcile_summary_labels(&mut overlapping, clip);
        assert_eq!(overlapping.len(), 1);

        let mut separated = (0..8)
            .map(|index| junction_label(index, 12.0 + index as f64 * 24.0, "1", index as u32))
            .collect::<Vec<_>>();
        reconcile_summary_labels(&mut separated, clip);
        assert_eq!(separated.len(), 8);
    }
}
