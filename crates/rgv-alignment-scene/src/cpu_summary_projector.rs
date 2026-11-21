use std::{collections::BTreeMap, mem::size_of, sync::Arc};

use rgv_core::{
    BisulfiteContext, CoverageScaleTransform, GenomicInterval, JunctionDisplayMode,
    SashimiCountMode, Viewport,
};
use rgv_model::{
    JunctionEvidence, ModificationBin, ModificationCode, ModificationSeriesKey, PairSummary,
    PileupBin,
};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentSceneSnapshot, AnnotatedJunctionSemantic, BisulfiteObservationToken,
    CompleteCoverage, CpuClipRect, JunctionAnnotationStatus, LedgerError, ReservationKind,
    SceneRevision, SummaryAlleleReferenceContext, SummaryPageFamily, SummarySemanticComponent,
    TransientResourceClass, TransientResourceReservation,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuSummaryCamera {
    viewport: Viewport,
    clip: CpuClipRect,
}

impl CpuSummaryCamera {
    pub fn new(
        interval: GenomicInterval,
        clip: CpuClipRect,
    ) -> Result<Self, CpuSummaryProjectionError> {
        validate_summary_camera_clip(clip)?;
        let viewport = Viewport::new(interval, clip.width)
            .map_err(|_| CpuSummaryProjectionError::InvalidCamera)?;
        Ok(Self { viewport, clip })
    }

    /// Builds a summary camera from the application's fractional genomic viewport.
    ///
    /// The enclosing integer interval remains the source/coverage authority, while
    /// `exact_start` and `bases_per_pixel` drive camera-only projection during a live pan.
    /// Resizing to the lane clip width preserves the genomic span and fractional origin.
    pub fn from_viewport(
        viewport: Viewport,
        clip: CpuClipRect,
    ) -> Result<Self, CpuSummaryProjectionError> {
        validate_summary_camera_clip(clip)?;
        let viewport = viewport
            .resized(clip.width)
            .map_err(|_| CpuSummaryProjectionError::InvalidCamera)?;
        Ok(Self { viewport, clip })
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
    pub const fn clip(self) -> CpuClipRect {
        self.clip
    }

    fn x(self, position: u64) -> f64 {
        self.clip.x + self.viewport.pixel_at_genomic(position)
    }
}

fn validate_summary_camera_clip(clip: CpuClipRect) -> Result<(), CpuSummaryProjectionError> {
    if !clip.x.is_finite()
        || !clip.y.is_finite()
        || !clip.width.is_finite()
        || clip.width <= 0.0
        || !clip.height.is_finite()
        || clip.height <= 0.0
    {
        return Err(CpuSummaryProjectionError::InvalidCamera);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuDepthProjectionOptions {
    pub scale_maximum: Option<u32>,
    pub transform: CoverageScaleTransform,
    pub show_center_line: bool,
    /// Exact genomic pileup position captured in the same panel epoch as this lane.
    pub selected_position: Option<u64>,
}

impl Default for CpuDepthProjectionOptions {
    fn default() -> Self {
        Self {
            scale_maximum: None,
            transform: CoverageScaleTransform::Linear,
            show_center_line: true,
            selected_position: None,
        }
    }
}

/// Biological interpretation of exact per-base coverage evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CoverageAlleleInterpretation {
    /// Emit substitutions only after excluding the exact FASTA reference base.
    #[default]
    GenericReferenceAware,
    /// Emit strand/context-specific protected and converted observations.
    Bisulfite(BisulfiteContext),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuAlleleProjectionOptions {
    pub minimum_fraction: f64,
    pub quality_weighted: bool,
    pub scale_maximum: Option<u32>,
    pub transform: CoverageScaleTransform,
    pub interpretation: CoverageAlleleInterpretation,
}

impl Default for CpuAlleleProjectionOptions {
    fn default() -> Self {
        Self {
            minimum_fraction: 0.2,
            quality_weighted: true,
            scale_maximum: None,
            transform: CoverageScaleTransform::Linear,
            interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
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

/// Camera-independent Sashimi scale measured directly from one immutable summary authority.
/// `maximum` uses the requested count mode and the same depth/flank filters as projection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuJunctionScaleMeasurement {
    pub maximum: f64,
    pub normalization_available: bool,
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
    SelectedPositionMarker,
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
pub enum CpuBisulfiteStrandToken {
    Forward,
    Reverse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuSummaryProjectedComponent {
    Depth {
        depth: u32,
        forward_depth: u32,
        reverse_depth: u32,
    },
    CoverageCenterLine,
    CoverageSelectedPosition {
        position: u64,
    },
    Allele {
        allele: CpuAlleleToken,
        observations: u64,
        fraction_bits: u64,
    },
    Bisulfite {
        observation: BisulfiteObservationToken,
        strand: CpuBisulfiteStrandToken,
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
    /// Measures the automatic junction maximum without constructing or scanning screen
    /// primitives. This is used to resolve same-epoch shared Sashimi scales before any member is
    /// projected.
    pub fn measure_junction_scale(
        scene: &AlignmentSceneSnapshot,
        interval: GenomicInterval,
        options: CpuJunctionProjectionOptions,
    ) -> Result<CpuJunctionScaleMeasurement, CpuSummaryProjectionError> {
        let maximum_facts = scene
            .summary_pages()
            .iter()
            .filter(|page| {
                page.key().family() == SummaryPageFamily::Junctions
                    && page.indexed_interval().overlaps(interval)
            })
            .fold(0_usize, |total, page| {
                total.saturating_add(page.primitives().len())
            });
        let _measurement_resources = scene.metadata_allocation().ledger().reserve_transient(
            TransientResourceClass::CpuSummaryProjection,
            cpu_summary_projection_scratch_bytes(maximum_facts, 0),
            ReservationKind::Normal,
        )?;
        let junctions = filtered_junctions(scene, interval, options);
        Ok(measure_filtered_junction_scale(
            scene,
            &junctions,
            options.count_mode,
        ))
    }

    pub fn project(
        scene: &Arc<AlignmentSceneSnapshot>,
        camera_sequence: u64,
        camera: CpuSummaryCamera,
        options: &CpuSummaryProjectionOptions,
    ) -> Result<CpuSummaryFrame, CpuSummaryProjectionError> {
        let covered = intersect(camera.interval(), scene.prepared_interval());
        let family = options.family();
        let maximum_facts = covered.map_or(0, |interval| {
            scene
                .summary_pages()
                .iter()
                .filter(|page| {
                    page.key().family() == family && page.indexed_interval().overlaps(interval)
                })
                .fold(0_usize, |total, page| {
                    total.saturating_add(page.primitives().len())
                })
        });
        let auxiliary_facts = covered.map_or(0, |interval| {
            (family == SummaryPageFamily::Alleles)
                .then(|| {
                    scene
                        .summary_pages()
                        .iter()
                        .filter(|page| {
                            page.key().family() == SummaryPageFamily::Depth
                                && page.indexed_interval().overlaps(interval)
                        })
                        .fold(0_usize, |total, page| {
                            total.saturating_add(page.primitives().len())
                        })
                })
                .unwrap_or_default()
        });
        let maximum_output = maximum_summary_output(maximum_facts, options);
        let mut projection_resources = scene.metadata_allocation().ledger().reserve_transient(
            TransientResourceClass::CpuSummaryProjection,
            cpu_summary_projection_scratch_bytes(
                maximum_facts.saturating_add(auxiliary_facts),
                maximum_output,
            ),
            ReservationKind::Normal,
        )?;
        let mut frame = CpuSummaryFrame {
            scene_revision: scene.revision(),
            camera_sequence,
            family,
            requested_interval: camera.interval(),
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

    /// Projects the deterministic dominant modification series for each visible semantic bin.
    /// The general low-level projection API intentionally retains every series; panel lanes use
    /// this compatibility projection so overlapping series cannot overpaint one another in an
    /// order-dependent way.
    pub fn project_dominant_modifications(
        scene: &Arc<AlignmentSceneSnapshot>,
        camera_sequence: u64,
        camera: CpuSummaryCamera,
        options: &CpuModificationProjectionOptions,
    ) -> Result<CpuSummaryFrame, CpuSummaryProjectionError> {
        let projection = CpuSummaryProjectionOptions::Modifications(options.clone());
        let mut frame = Self::project(scene, camera_sequence, camera, &projection)?;
        retain_dominant_modification_primitives(&mut frame);
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
        let fraction = transformed_depth_fraction(depth, maximum, options.transform);
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
    project_coverage_navigation_markers(camera, options, frame);
}

fn project_coverage_navigation_markers(
    camera: CpuSummaryCamera,
    options: CpuDepthProjectionOptions,
    frame: &mut CpuSummaryFrame,
) {
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
    if let Some(position) = options
        .selected_position
        .filter(|position| camera.interval().contains(*position))
    {
        let base_width = (1.0 / camera.bases_per_pixel()).clamp(2.0, 12.0);
        push(
            frame,
            camera.x(position),
            camera.clip.y,
            base_width,
            camera.clip.height,
            CpuSummaryPrimitiveShape::SelectedPositionMarker,
            CpuSummaryProjectedComponent::CoverageSelectedPosition { position },
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
    let mut depth_facts = summary_facts(scene, SummaryPageFamily::Depth, covered);
    depth_facts.sort_unstable_by_key(|(start, end, _)| (*start, *end));
    let automatic = depth_facts
        .iter()
        .filter_map(|(_, _, component)| match component {
            SummarySemanticComponent::Depth { depth, .. } => Some(*depth),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    let maximum = options.scale_maximum.unwrap_or(automatic).max(1);
    frame.scale_maximum = Some(f64::from(maximum));
    let mut allele_facts = allele_summary_facts(scene, covered);
    allele_facts.sort_unstable_by_key(|(start, end, _, _)| (*start, *end));
    let mut depth_cursor = 0;
    for (start, end, component, reference_context) in allele_facts {
        let SummarySemanticComponent::Alleles {
            bin,
            reference_base,
        } = component
        else {
            continue;
        };
        project_allele_bin(
            frame,
            camera,
            start,
            end,
            bin,
            reference_base,
            reference_context,
            options,
            local_depth(&depth_facts, &mut depth_cursor, start, end),
            maximum,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn project_allele_bin(
    frame: &mut CpuSummaryFrame,
    camera: CpuSummaryCamera,
    start: u64,
    end: u64,
    bin: PileupBin,
    reference_base: Option<u8>,
    reference_context: Option<SummaryAlleleReferenceContext>,
    options: CpuAlleleProjectionOptions,
    depth: u32,
    depth_maximum: u32,
) {
    if depth == 0 {
        return;
    }
    match options.interpretation {
        CoverageAlleleInterpretation::GenericReferenceAware => project_generic_allele_bin(
            frame,
            camera,
            start,
            end,
            bin,
            reference_base,
            options,
            depth,
            depth_maximum,
        ),
        CoverageAlleleInterpretation::Bisulfite(context) => project_bisulfite_allele_bin(
            frame,
            camera,
            start,
            end,
            bin,
            reference_context,
            context,
            options,
            depth,
            depth_maximum,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn project_generic_allele_bin(
    frame: &mut CpuSummaryFrame,
    camera: CpuSummaryCamera,
    start: u64,
    end: u64,
    bin: PileupBin,
    reference_base: Option<u8>,
    options: CpuAlleleProjectionOptions,
    depth: u32,
    depth_maximum: u32,
) {
    let Some(reference_base) = reference_base.map(|base| base.to_ascii_uppercase()) else {
        return;
    };
    let denominator = if options.quality_weighted {
        bin.quality_weighted.total()
    } else {
        u64::from(bin.alleles.base_observations())
    };
    if denominator == 0 {
        return;
    }
    let alternates = [
        (CpuAlleleToken::A, b'A'),
        (CpuAlleleToken::C, b'C'),
        (CpuAlleleToken::G, b'G'),
        (CpuAlleleToken::T, b'T'),
        (CpuAlleleToken::N, b'N'),
    ]
    .map(|(allele, base)| {
        let observations = if base == reference_base {
            0
        } else if options.quality_weighted {
            bin.quality_weighted.for_base(base)
        } else {
            u64::from(bin.alleles.for_base(base))
        };
        (allele, observations)
    });
    let alternate_observations = alternates.iter().fold(0_u64, |total, (_, observations)| {
        total.saturating_add(*observations)
    });
    let alternate_fraction = alternate_observations as f64 / denominator as f64;
    if alternate_observations == 0 || alternate_fraction < options.minimum_fraction {
        return;
    }

    let coverage_fraction =
        transformed_depth_fraction(depth, depth_maximum.max(1), options.transform);
    let coverage_height = (coverage_fraction * camera.clip.height).max(1.0);
    let mut cursor = camera.clip.y + camera.clip.height;
    for (allele, observations) in alternates {
        if observations == 0 {
            continue;
        }
        cursor = push_allele_segment(
            frame,
            camera,
            start,
            end,
            allele,
            observations,
            denominator,
            coverage_height,
            cursor,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn project_bisulfite_allele_bin(
    frame: &mut CpuSummaryFrame,
    camera: CpuSummaryCamera,
    start: u64,
    end: u64,
    bin: PileupBin,
    reference_context: Option<SummaryAlleleReferenceContext>,
    context: BisulfiteContext,
    options: CpuAlleleProjectionOptions,
    depth: u32,
    depth_maximum: u32,
) {
    let Some(reference_context) = reference_context else {
        return;
    };
    let (alleles, weighted, strand, protected_base, converted_base) =
        if reference_context.matches(false, context) {
            (
                bin.bisulfite_forward_alleles,
                bin.bisulfite_forward_quality_weighted,
                CpuBisulfiteStrandToken::Forward,
                b'C',
                b'T',
            )
        } else if reference_context.matches(true, context) {
            (
                bin.bisulfite_reverse_alleles,
                bin.bisulfite_reverse_quality_weighted,
                CpuBisulfiteStrandToken::Reverse,
                b'G',
                b'A',
            )
        } else {
            return;
        };
    // Legacy bisulfite coverage uses the complete base-observation denominator while the
    // numerator is restricted to the biological template strand selected by exact context.
    let denominator = if options.quality_weighted {
        bin.quality_weighted.total()
    } else {
        u64::from(bin.alleles.base_observations())
    };
    if denominator == 0 {
        return;
    }
    let coverage_fraction =
        transformed_depth_fraction(depth, depth_maximum.max(1), options.transform);
    let coverage_height = (coverage_fraction * camera.clip.height).max(1.0);
    let mut cursor = camera.clip.y + camera.clip.height;
    for (observation, base) in [
        (BisulfiteObservationToken::Protected, protected_base),
        (BisulfiteObservationToken::Converted, converted_base),
    ] {
        let observations = if options.quality_weighted {
            weighted.for_base(base)
        } else {
            u64::from(alleles.for_base(base))
        };
        if observations == 0 {
            continue;
        }
        let fraction = observations as f64 / denominator as f64;
        let semantic_height = fraction * coverage_height;
        let y = (cursor - semantic_height).max(camera.clip.y);
        let (x, width) = horizontal(camera, start, end);
        push(
            frame,
            x,
            y,
            width,
            semantic_height.max(1.0).min(coverage_height),
            CpuSummaryPrimitiveShape::Bar,
            CpuSummaryProjectedComponent::Bisulfite {
                observation,
                strand,
                observations,
                fraction_bits: fraction.to_bits(),
            },
            None,
        );
        cursor = y;
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
    coverage_height: f64,
    cursor: f64,
) -> f64 {
    let fraction = observations as f64 / denominator as f64;
    let semantic_height = fraction * coverage_height;
    let y = (cursor - semantic_height).max(camera.clip.y);
    let (x, width) = horizontal(camera, start, end);
    push(
        frame,
        x,
        y,
        width,
        semantic_height.max(1.0).min(coverage_height),
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

fn transformed_depth_fraction(depth: u32, maximum: u32, transform: CoverageScaleTransform) -> f64 {
    let fraction = (f64::from(depth) / f64::from(maximum.max(1))).min(1.0);
    match transform {
        CoverageScaleTransform::Linear => fraction,
        CoverageScaleTransform::SquareRoot => fraction.sqrt(),
    }
}

fn local_depth(
    depth_facts: &[(u64, u64, SummarySemanticComponent)],
    cursor: &mut usize,
    start: u64,
    end: u64,
) -> u32 {
    while depth_facts
        .get(*cursor)
        .is_some_and(|(_, depth_end, _)| *depth_end <= start)
    {
        *cursor = (*cursor).saturating_add(1);
    }
    depth_facts[*cursor..]
        .iter()
        .take_while(|(depth_start, _, _)| *depth_start < end)
        .filter_map(|(_, _, component)| match component {
            SummarySemanticComponent::Depth { depth, .. } => Some(*depth),
            _ => None,
        })
        .max()
        .unwrap_or_default()
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
    let junctions = filtered_junctions(scene, covered, options);
    let measurement = measure_filtered_junction_scale(scene, &junctions, options.count_mode);
    let normalization = junction_normalization(scene);
    frame.normalization_available = measurement.normalization_available;
    let maximum = options.scale_maximum.unwrap_or(measurement.maximum);
    frame.scale_maximum = Some(maximum);
    let rows = if options.display_mode == JunctionDisplayMode::Expanded {
        junctions
            .iter()
            .map(|junction| junction.expanded_row)
            .collect()
    } else {
        vec![0; junctions.len()]
    };
    // Expanded row identity belongs to the complete immutable summary root, not the visible
    // slice. Keeping the root's row count prevents the same junction from moving vertically when
    // a pan changes which neighbouring arcs happen to be on screen.
    let row_count = if options.display_mode == JunctionDisplayMode::Expanded {
        scene.summaries().expanded_junction_row_count().max(1)
    } else {
        1
    };
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
        // Arcs are parameterised over the junction's full, unclamped span (mirror of
        // `junction_vertex` in `alignment_summary.wgsl`), so their bounds may extend beyond the
        // camera clip and consumers must clip them to the lane. Clamping the span instead would
        // squeeze the parabola into the visible part and make an off-screen end appear to land
        // on the viewport edge. Markers and labels stay anchored to the visible span.
        let (arc_x, arc_width) = horizontal_unclamped(camera, junction.start, junction.end);
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
                arc_x,
                if up { baseline - height } else { baseline },
                arc_width,
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

fn filtered_junctions(
    scene: &AlignmentSceneSnapshot,
    interval: GenomicInterval,
    options: CpuJunctionProjectionOptions,
) -> Vec<AnnotatedJunctionSemantic> {
    let mut junctions = summary_facts(scene, SummaryPageFamily::Junctions, interval)
        .into_iter()
        .filter_map(|(_, _, component)| match component {
            SummarySemanticComponent::Junction(junction)
                if junction.evidence.depth >= options.minimum_depth
                    && junction.evidence.minimum_left_flank >= options.minimum_flank
                    && junction.evidence.minimum_right_flank >= options.minimum_flank =>
            {
                Some(junction)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    junctions.sort_unstable_by_key(|junction| (junction.evidence.start, junction.evidence.end));
    junctions
}

fn measure_filtered_junction_scale(
    scene: &AlignmentSceneSnapshot,
    junctions: &[AnnotatedJunctionSemantic],
    count_mode: SashimiCountMode,
) -> CpuJunctionScaleMeasurement {
    let normalization = junction_normalization(scene);
    let normalization_available =
        count_mode == SashimiCountMode::Raw || normalization.is_some_and(|total| total > 0);
    let maximum = junctions
        .iter()
        .map(|junction| junction_value(junction.evidence, count_mode, normalization))
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);
    CpuJunctionScaleMeasurement {
        maximum,
        normalization_available,
    }
}

fn junction_normalization(scene: &AlignmentSceneSnapshot) -> Option<u64> {
    scene
        .summaries()
        .junction_tiles()
        .iter()
        .find_map(|tile| tile.summary().provenance.normalization_mapped_records)
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

fn retain_dominant_modification_primitives(frame: &mut CpuSummaryFrame) {
    let mut dominant = BTreeMap::<(u64, u64), CpuSummaryProjectedPrimitive>::new();
    for primitive in frame.primitives.iter() {
        let CpuSummaryProjectedComponent::Modification {
            series,
            bin,
            fraction_bits,
        } = primitive.component
        else {
            continue;
        };
        if bin.modified_calls == 0 {
            continue;
        }
        let key = (primitive.x.to_bits(), primitive.width.to_bits());
        let replace = dominant.get(&key).is_none_or(|current| {
            let CpuSummaryProjectedComponent::Modification {
                series: current_series,
                fraction_bits: current_fraction_bits,
                ..
            } = current.component
            else {
                return true;
            };
            let fraction = f64::from_bits(fraction_bits);
            let current_fraction = f64::from_bits(current_fraction_bits);
            fraction.total_cmp(&current_fraction).is_gt()
                || (fraction.to_bits() == current_fraction.to_bits() && series < current_series)
        });
        if replace {
            dominant.insert(key, primitive.clone());
        }
    }
    let mut primitives = dominant.into_values().collect::<Vec<_>>();
    primitives.sort_by(|left, right| {
        left.y
            .total_cmp(&right.y)
            .then_with(|| left.x.total_cmp(&right.x))
            .then_with(|| left.draw_order.cmp(&right.draw_order))
    });
    for (draw_order, primitive) in primitives.iter_mut().enumerate() {
        primitive.draw_order = u32::try_from(draw_order).unwrap_or(u32::MAX);
    }
    frame.primitives = Arc::new(primitives);
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

fn allele_summary_facts(
    scene: &AlignmentSceneSnapshot,
    interval: GenomicInterval,
) -> Vec<(
    u64,
    u64,
    SummarySemanticComponent,
    Option<SummaryAlleleReferenceContext>,
)> {
    scene
        .summary_pages_for(SummaryPageFamily::Alleles, interval)
        .into_iter()
        .flat_map(|page| {
            page.primitives()
                .iter()
                .enumerate()
                .filter_map(move |(index, primitive)| {
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
                    (start < end).then_some((
                        start,
                        end,
                        primitive.component,
                        page.allele_reference_contexts()
                            .and_then(|contexts| contexts.get(index))
                            .copied(),
                    ))
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
        CpuSummaryProjectionOptions::Depth(_) => facts.saturating_add(2),
        CpuSummaryProjectionOptions::Alleles(_) => facts.saturating_mul(5),
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
    let start = start.max(camera.interval().start());
    let end = end.min(camera.interval().end());
    let x = camera.x(start);
    (x, (camera.x(end) - x).max(1.0))
}

/// Projects a genomic span without clamping it to the camera interval.
fn horizontal_unclamped(camera: CpuSummaryCamera, start: u64, end: u64) -> (f64, f64) {
    let x = camera.x(start);
    (x, (camera.x(end) - x).max(1.0))
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
    use rgv_core::ContigId;
    use rgv_model::{AlleleSupport, QualityWeightedAlleleSupport, ReferenceSlice};

    use super::*;
    use crate::SummarySemanticPrimitive;

    fn camera_clip() -> CpuClipRect {
        CpuClipRect {
            x: 17.0,
            y: 3.0,
            width: 1_000.0,
            height: 40.0,
        }
    }

    fn allele_options(minimum_fraction: f64) -> CpuAlleleProjectionOptions {
        CpuAlleleProjectionOptions {
            minimum_fraction,
            quality_weighted: false,
            scale_maximum: Some(100),
            transform: CoverageScaleTransform::Linear,
            interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
        }
    }

    fn allele_frame(camera: CpuSummaryCamera) -> CpuSummaryFrame {
        CpuSummaryFrame {
            scene_revision: SceneRevision::default(),
            camera_sequence: 0,
            family: SummaryPageFamily::Alleles,
            requested_interval: camera.interval(),
            covered_interval: Some(camera.interval()),
            scale_maximum: None,
            normalization_available: true,
            primitives: Arc::new(Vec::new()),
            _resources: None,
        }
    }

    fn project_test_allele_bin(
        bin: PileupBin,
        reference_base: Option<u8>,
        options: CpuAlleleProjectionOptions,
    ) -> CpuSummaryFrame {
        project_test_allele_bin_at_depth(bin, reference_base, options, 100)
    }

    fn project_test_allele_bin_at_depth(
        bin: PileupBin,
        reference_base: Option<u8>,
        options: CpuAlleleProjectionOptions,
        depth: u32,
    ) -> CpuSummaryFrame {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
        let camera = CpuSummaryCamera::new(interval, camera_clip()).unwrap();
        let mut frame = allele_frame(camera);
        project_allele_bin(
            &mut frame,
            camera,
            105,
            106,
            bin,
            reference_base,
            None,
            options,
            depth,
            options.scale_maximum.unwrap_or(100),
        );
        frame
    }

    fn projected_alleles(frame: &CpuSummaryFrame) -> Vec<(CpuAlleleToken, u64, f64, f64, f64)> {
        frame
            .primitives
            .iter()
            .filter_map(|primitive| match primitive.component {
                CpuSummaryProjectedComponent::Allele {
                    allele,
                    observations,
                    fraction_bits,
                } => Some((
                    allele,
                    observations,
                    f64::from_bits(fraction_bits),
                    primitive.y,
                    primitive.height,
                )),
                _ => None,
            })
            .collect()
    }

    fn projected_bisulfite(
        frame: &CpuSummaryFrame,
    ) -> Vec<(
        BisulfiteObservationToken,
        CpuBisulfiteStrandToken,
        u64,
        f64,
        f64,
    )> {
        frame
            .primitives
            .iter()
            .filter_map(|primitive| match primitive.component {
                CpuSummaryProjectedComponent::Bisulfite {
                    observation,
                    strand,
                    observations,
                    fraction_bits,
                } => Some((
                    observation,
                    strand,
                    observations,
                    f64::from_bits(fraction_bits),
                    primitive.height,
                )),
                _ => None,
            })
            .collect()
    }

    fn project_test_bisulfite_bin(
        position: u64,
        bin: PileupBin,
        reference: &ReferenceSlice,
        quality_weighted: bool,
    ) -> CpuSummaryFrame {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
        let camera = CpuSummaryCamera::new(interval, camera_clip()).unwrap();
        let mut frame = allele_frame(camera);
        let options = CpuAlleleProjectionOptions {
            // Bisulfite legacy semantics deliberately do not apply the generic substitution
            // threshold.
            minimum_fraction: 0.99,
            quality_weighted,
            scale_maximum: Some(4),
            transform: CoverageScaleTransform::Linear,
            interpretation: CoverageAlleleInterpretation::Bisulfite(BisulfiteContext::Cg),
        };
        project_allele_bin(
            &mut frame,
            camera,
            position,
            position + 1,
            bin,
            reference.base_at(position),
            Some(SummaryAlleleReferenceContext::capture(reference, position)),
            options,
            4,
            4,
        );
        frame
    }

    #[test]
    fn integer_interval_constructor_preserves_the_existing_summary_camera_mapping() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
        let camera = CpuSummaryCamera::new(interval, camera_clip()).unwrap();

        assert_eq!(camera.interval(), interval);
        assert_eq!(camera.viewport().interval(), interval);
        assert!((camera.exact_start() - 100.0).abs() < f64::EPSILON);
        assert!((camera.span() - 10.0).abs() < f64::EPSILON);
        assert!((camera.bases_per_pixel() - 0.01).abs() < f64::EPSILON);
        assert!((camera.x(100) - 17.0).abs() < f64::EPSILON);
        assert!((camera.x(105) - 517.0).abs() < 1.0e-9);
        assert!((camera.x(110) - 1_017.0).abs() < 1.0e-9);
    }

    #[test]
    fn fractional_summary_pan_moves_pixels_without_changing_semantic_authority() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
        let mut viewport = Viewport::new(interval, camera_clip().width).unwrap();
        let semantic = SummarySemanticPrimitive {
            start_offset: 5,
            end_offset: 6,
            component: SummarySemanticComponent::Depth {
                depth: 42,
                forward_depth: 20,
                reverse_depth: 22,
                mapping_quality_sum: 1_260,
                mapping_quality_observations: 42,
            },
        };
        let semantic_authority = semantic;
        let first_camera = CpuSummaryCamera::from_viewport(viewport, camera_clip()).unwrap();
        let (first_x, first_width) = horizontal(
            first_camera,
            interval.start() + semantic.start_offset,
            interval.start() + semantic.end_offset,
        );

        for step in 1..=25 {
            viewport = viewport.pan_pixels(1.0, 10_000).unwrap();
            let camera = CpuSummaryCamera::from_viewport(viewport, camera_clip()).unwrap();
            let (x, width) = horizontal(
                camera,
                interval.start() + semantic.start_offset,
                interval.start() + semantic.end_offset,
            );

            assert!((camera.exact_start() - (100.0 + f64::from(step) * 0.01)).abs() < 1.0e-9);
            assert!((x - (first_x - f64::from(step))).abs() < 1.0e-8);
            assert!((width - first_width).abs() < 1.0e-8);
            assert_eq!(semantic, semantic_authority);
        }
    }

    #[test]
    fn coverage_selected_position_is_an_exact_typed_full_height_marker() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
        let camera = CpuSummaryCamera::new(interval, camera_clip()).unwrap();
        let mut frame = CpuSummaryFrame {
            scene_revision: SceneRevision::default(),
            camera_sequence: 7,
            family: SummaryPageFamily::Depth,
            requested_interval: interval,
            covered_interval: Some(interval),
            scale_maximum: None,
            normalization_available: true,
            primitives: Arc::new(Vec::new()),
            _resources: None,
        };
        project_coverage_navigation_markers(
            camera,
            CpuDepthProjectionOptions {
                show_center_line: false,
                selected_position: Some(105),
                ..CpuDepthProjectionOptions::default()
            },
            &mut frame,
        );

        assert_eq!(frame.primitives.len(), 1);
        let marker = &frame.primitives[0];
        assert_eq!(
            marker.shape,
            CpuSummaryPrimitiveShape::SelectedPositionMarker
        );
        assert_eq!(
            marker.component,
            CpuSummaryProjectedComponent::CoverageSelectedPosition { position: 105 }
        );
        assert!((marker.x - camera.x(105)).abs() < 1.0e-9);
        assert!((marker.width - 12.0).abs() < 1.0e-9);
        assert_eq!(marker.y, camera.clip.y);
        assert_eq!(marker.height, camera.clip.height);

        Arc::make_mut(&mut frame.primitives).clear();
        project_coverage_navigation_markers(
            camera,
            CpuDepthProjectionOptions {
                show_center_line: false,
                selected_position: Some(interval.end()),
                ..CpuDepthProjectionOptions::default()
            },
            &mut frame,
        );
        assert!(frame.primitives.is_empty());
    }

    #[test]
    fn bisulfite_projection_matches_legacy_forward_and_reverse_context_support() {
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 100, 102).unwrap(),
            bases: b"CG".to_vec(),
        };
        let forward = PileupBin {
            depth: 4,
            alleles: AlleleSupport {
                a: 1,
                c: 2,
                t: 1,
                ..AlleleSupport::default()
            },
            bisulfite_forward_alleles: AlleleSupport {
                c: 2,
                t: 1,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };
        let reverse = PileupBin {
            depth: 4,
            alleles: AlleleSupport {
                a: 2,
                c: 1,
                g: 1,
                ..AlleleSupport::default()
            },
            bisulfite_reverse_alleles: AlleleSupport {
                a: 2,
                g: 1,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };

        let forward =
            projected_bisulfite(&project_test_bisulfite_bin(100, forward, &reference, false));
        assert_eq!(
            forward,
            vec![
                (
                    BisulfiteObservationToken::Protected,
                    CpuBisulfiteStrandToken::Forward,
                    2,
                    0.5,
                    20.0,
                ),
                (
                    BisulfiteObservationToken::Converted,
                    CpuBisulfiteStrandToken::Forward,
                    1,
                    0.25,
                    10.0,
                ),
            ]
        );
        let reverse =
            projected_bisulfite(&project_test_bisulfite_bin(101, reverse, &reference, false));
        assert_eq!(
            reverse,
            vec![
                (
                    BisulfiteObservationToken::Protected,
                    CpuBisulfiteStrandToken::Reverse,
                    1,
                    0.25,
                    10.0,
                ),
                (
                    BisulfiteObservationToken::Converted,
                    CpuBisulfiteStrandToken::Reverse,
                    2,
                    0.5,
                    20.0,
                ),
            ]
        );
    }

    #[test]
    fn bisulfite_projection_suppresses_noncontext_substitutions_and_uses_weighted_strand_support() {
        let non_context = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 100, 102).unwrap(),
            bases: b"CA".to_vec(),
        };
        let evidence = PileupBin {
            depth: 4,
            alleles: AlleleSupport {
                c: 2,
                t: 2,
                ..AlleleSupport::default()
            },
            bisulfite_forward_alleles: AlleleSupport {
                c: 2,
                t: 2,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };
        let frame = project_test_bisulfite_bin(100, evidence, &non_context, false);
        assert!(frame.primitives.is_empty());

        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 100, 102).unwrap(),
            bases: b"CG".to_vec(),
        };
        let weighted = PileupBin {
            depth: 4,
            alleles: AlleleSupport {
                c: 2,
                t: 2,
                ..AlleleSupport::default()
            },
            quality_weighted: QualityWeightedAlleleSupport {
                c: 60,
                t: 40,
                ..QualityWeightedAlleleSupport::default()
            },
            bisulfite_forward_quality_weighted: QualityWeightedAlleleSupport {
                c: 30,
                t: 20,
                ..QualityWeightedAlleleSupport::default()
            },
            ..PileupBin::default()
        };
        let projected =
            projected_bisulfite(&project_test_bisulfite_bin(100, weighted, &reference, true));
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].2, 30);
        assert!((projected[0].3 - 0.3).abs() < 1.0e-9);
        assert_eq!(projected[1].2, 20);
        assert!((projected[1].3 - 0.2).abs() < 1.0e-9);
        assert!(
            projected
                .iter()
                .all(|(_, strand, ..)| { *strand == CpuBisulfiteStrandToken::Forward })
        );
    }

    #[test]
    fn allele_projection_excludes_the_reference_base_and_indels() {
        let bin = PileupBin {
            depth: 100,
            alleles: AlleleSupport {
                a: 80,
                c: 10,
                g: 10,
                deletion: 500,
                insertion: 500,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };
        let frame = project_test_allele_bin(bin, Some(b'a'), allele_options(0.0));
        let alleles = projected_alleles(&frame);

        assert_eq!(alleles.len(), 2);
        assert_eq!(alleles[0].0, CpuAlleleToken::C);
        assert_eq!(alleles[0].1, 10);
        assert_eq!(alleles[1].0, CpuAlleleToken::G);
        assert_eq!(alleles[1].1, 10);
        assert!(alleles.iter().all(|(allele, ..)| !matches!(
            allele,
            CpuAlleleToken::A | CpuAlleleToken::Deletion | CpuAlleleToken::Insertion
        )));
    }

    #[test]
    fn allele_threshold_applies_to_the_aggregate_alternate_fraction() {
        let bin = PileupBin {
            depth: 100,
            alleles: AlleleSupport {
                a: 80,
                c: 10,
                g: 10,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };

        let at_threshold = projected_alleles(&project_test_allele_bin(
            bin,
            Some(b'A'),
            allele_options(0.2),
        ));
        assert_eq!(at_threshold.len(), 2);
        assert!(
            at_threshold
                .iter()
                .all(|(_, _, fraction, ..)| (*fraction - 0.1).abs() < 1.0e-9)
        );

        let above_threshold = projected_alleles(&project_test_allele_bin(
            bin,
            Some(b'A'),
            allele_options(0.200_001),
        ));
        assert!(above_threshold.is_empty());
    }

    #[test]
    fn allele_projection_requires_an_exact_reference_base() {
        let bin = PileupBin {
            depth: 100,
            alleles: AlleleSupport {
                a: 50,
                c: 50,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };

        let frame = project_test_allele_bin(bin, None, allele_options(0.0));
        assert!(projected_alleles(&frame).is_empty());
    }

    #[test]
    fn allele_segments_are_scaled_inside_the_local_coverage_height() {
        // Allele tiles intentionally own no depth. The co-published depth page supplies 25x.
        let bin = PileupBin {
            alleles: AlleleSupport {
                a: 50,
                c: 50,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        };
        let linear = projected_alleles(&project_test_allele_bin_at_depth(
            bin,
            Some(b'A'),
            allele_options(0.0),
            25,
        ));
        assert_eq!(linear.len(), 1);
        assert!((linear[0].4 - 5.0).abs() < 1.0e-9);
        assert!((linear[0].3 - 38.0).abs() < 1.0e-9);

        let square_root = projected_alleles(&project_test_allele_bin_at_depth(
            bin,
            Some(b'A'),
            CpuAlleleProjectionOptions {
                transform: CoverageScaleTransform::SquareRoot,
                ..allele_options(0.0)
            },
            25,
        ));
        assert_eq!(square_root.len(), 1);
        assert!((square_root[0].4 - 10.0).abs() < 1.0e-9);
        assert!((square_root[0].3 - 33.0).abs() < 1.0e-9);

        let no_local_depth = projected_alleles(&project_test_allele_bin_at_depth(
            bin,
            Some(b'A'),
            allele_options(0.0),
            0,
        ));
        assert!(no_local_depth.is_empty());
    }

    #[test]
    fn quality_weighted_allele_threshold_uses_weighted_aggregate_support() {
        let bin = PileupBin {
            depth: 100,
            alleles: AlleleSupport {
                a: 80,
                c: 10,
                g: 10,
                ..AlleleSupport::default()
            },
            quality_weighted: QualityWeightedAlleleSupport {
                a: 90,
                c: 5,
                g: 5,
                ..QualityWeightedAlleleSupport::default()
            },
            ..PileupBin::default()
        };
        let raw = projected_alleles(&project_test_allele_bin(
            bin,
            Some(b'A'),
            allele_options(0.15),
        ));
        assert_eq!(raw.len(), 2);

        let weighted = projected_alleles(&project_test_allele_bin(
            bin,
            Some(b'A'),
            CpuAlleleProjectionOptions {
                quality_weighted: true,
                ..allele_options(0.15)
            },
        ));
        assert!(weighted.is_empty());
    }

    #[test]
    fn cpu_summary_unclamped_span_keeps_off_screen_junction_ends_off_screen() {
        // 100 bases across 256 points.
        let interval = GenomicInterval::new(ContigId::new(0), 1_000, 1_100).unwrap();
        let camera = CpuSummaryCamera::new(interval, camera_clip()).unwrap();
        let clip = camera.clip();
        let points_per_base = clip.width / 100.0;

        // Wholly visible: identical to the clamped projection.
        assert_eq!(
            horizontal_unclamped(camera, 1_010, 1_050),
            horizontal(camera, 1_010, 1_050)
        );

        // Starts 50 bases left of the pane and ends 50 bases inside it. The clamped span used
        // for bars collapses onto the pane; the arc span keeps its true start and width, so the
        // pane edge sits at the middle of the arc.
        let (clamped_x, clamped_width) = horizontal(camera, 950, 1_050);
        assert!((clamped_x - clip.x).abs() < 1.0e-9);
        assert!((clamped_width - 50.0 * points_per_base).abs() < 1.0e-9);
        let (x, width) = horizontal_unclamped(camera, 950, 1_050);
        assert!((x - (clip.x - 50.0 * points_per_base)).abs() < 1.0e-9);
        assert!((width - 100.0 * points_per_base).abs() < 1.0e-9);
        assert!(((clip.x - x) / width - 0.5).abs() < 1.0e-9);

        // Both ends off-screen: the arc spans the whole pane and beyond on both sides.
        let (x, width) = horizontal_unclamped(camera, 900, 1_300);
        assert!(x < clip.x && x + width > clip.x + clip.width);
        assert!((width - 400.0 * points_per_base).abs() < 1.0e-9);

        // Sub-point junctions keep the one-point minimum.
        let wide = CpuSummaryCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap(),
            camera_clip(),
        )
        .unwrap();
        assert!((horizontal_unclamped(wide, 500, 510).1 - 1.0).abs() < 1.0e-9);
    }

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
