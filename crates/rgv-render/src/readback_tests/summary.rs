use std::sync::Arc;

use rgv_alignment_scene::{
    AlignmentSashimiScalePolicy, AlignmentSceneSnapshot, AllocationLedger, CpuClipRect,
    CpuSummaryCamera, SummaryPageFamily, SummarySemanticPage,
};
use rgv_core::{
    ContigId, CoverageScaleTransform, GenomicInterval, JunctionDisplayMode, ModifiedBaseColorMode,
    Viewport,
};

use super::{EDGE_BAND, FORMAT, Oracle, Quad, Shade, TestGpu, render_image, smoothstep, test_gpu};
use crate::{
    AlignmentGpuScissor, AlignmentSummaryFrameOverlays, AlignmentSummaryGpuDepthInstance,
    AlignmentSummaryGpuLaneOptions, AlignmentSummaryGpuPageRequest, AlignmentSummaryGpuRenderer,
    AlignmentSummaryGpuSceneRequest, AlignmentSummaryJunctionComponentToken,
    PackedAlignmentSummaryPage,
    alignment_summary::tests::{
        lane_options, modification_options, sashimi_options, test_snapshot,
        test_snapshot_with_reference,
    },
    palette::BASE_COLORS,
    project_alignment_summary_allele_instance, project_alignment_summary_depth_instance,
    project_alignment_summary_junction_instance_with_labels,
    project_alignment_summary_modification_instance,
};

const TARGET: [u32; 2] = [300, 120];
/// Off the pixel grid, so lane edges do not sit on pixel centres.
const CLIP: CpuClipRect = CpuClipRect {
    x: 16.4,
    y: 8.3,
    width: 256.0,
    height: 96.0,
};
/// Encloses the clip: the summary vertex stage clamps to the lane itself.
const SCISSOR: AlignmentGpuScissor = AlignmentGpuScissor {
    x: 16,
    y: 8,
    width: 257,
    height: 97,
};
const DEPTH_COLOUR: [f32; 4] = [0.48, 0.48, 0.48, 1.0];

const JUNCTION_BASELINE_COLOUR: [f32; 4] = [0.36, 0.38, 0.42, 0.24];

fn scissor() -> [u32; 4] {
    [SCISSOR.x, SCISSOR.y, SCISSOR.width, SCISSOR.height]
}

/// About thirteen pixels per base over the fixture read (10..23, spliced 15..18), panned by a
/// fraction of a base so the camera origin has a sub-base part.
fn camera() -> CpuSummaryCamera {
    CpuSummaryCamera::from_viewport(
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 6, 26).unwrap(),
            CLIP.width,
        )
        .unwrap()
        .pan_pixels(4.37, 100)
        .unwrap(),
        CLIP,
    )
    .unwrap()
}

fn pages(
    scene: &Arc<AlignmentSceneSnapshot>,
    family: SummaryPageFamily,
) -> Vec<&Arc<SummarySemanticPage>> {
    scene
        .summary_pages()
        .iter()
        .filter(|page| page.key().family() == family)
        .collect()
}

fn render_lane(
    gpu: &TestGpu,
    renderer: &mut AlignmentSummaryGpuRenderer,
    scene: &Arc<AlignmentSceneSnapshot>,
    ledger: &AllocationLedger,
    options: AlignmentSummaryGpuLaneOptions,
    sequence: u64,
) -> image::RgbaImage {
    let requests = [AlignmentSummaryGpuSceneRequest {
        scene,
        coherence_scope: 1,
        lane_identity: 1,
        camera_sequence: sequence,
        camera: camera(),
        screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
        scissor: SCISSOR,
        options,
        overlays: AlignmentSummaryFrameOverlays::default(),
        junction_presentation: None,
    }];
    let (frame, _, _) = renderer
        .prepare_with_admission(&gpu.device, &gpu.queue, &requests, ledger, sequence)
        .unwrap();
    render_image(gpu, TARGET, |pass| renderer.render(pass, &frame))
}

fn quad(bounds: crate::AlignmentSummaryGpuProjectedBounds) -> Quad {
    Quad {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
    }
}

fn paint_solid(oracle: &mut Oracle, quad: Quad, colour: [f32; 4]) {
    let colour = colour.map(f64::from);
    oracle.paint(quad, scissor(), |x, y| match quad.classify(x, y) {
        Some(true) => Shade::Colour(colour),
        Some(false) => Shade::Outside,
        None => Shade::Uncertain,
    });
}

fn paint_depth(
    oracle: &mut Oracle,
    scene: &Arc<AlignmentSceneSnapshot>,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> usize {
    let mut painted = 0;
    for page in pages(scene, SummaryPageFamily::Depth) {
        let PackedAlignmentSummaryPage::Depth { instances, .. } =
            PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::depth(page))
                .unwrap()
        else {
            unreachable!();
        };
        for instance in instances {
            if let Some(bounds) = project_alignment_summary_depth_instance(
                instance,
                page.interval().start(),
                camera(),
                scale_maximum,
                transform,
            ) {
                paint_solid(oracle, quad(bounds), DEPTH_COLOUR);
                painted += 1;
            }
        }
    }
    painted
}

fn report(label: &str, gpu: &TestGpu, instances: usize, comparison: &super::Comparison) {
    eprintln!(
        "{label} on {}: {instances} instances, {} pixels compared, {} painted, {} boundary pixels \
         excluded",
        gpu.description, comparison.compared, comparison.painted, comparison.uncertain
    );
}

#[test]
fn summary_depth_pixels_match_the_cpu_oracle_for_both_scale_transforms() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer = AlignmentSummaryGpuRenderer::new(&gpu.device, FORMAT);
    for (sequence, (scale_maximum, transform)) in [
        (2, CoverageScaleTransform::Linear),
        (5, CoverageScaleTransform::SquareRoot),
    ]
    .into_iter()
    .enumerate()
    {
        let mut options = lane_options(false);
        options.depth.scale_maximum = Some(scale_maximum);
        options.depth.transform = transform;
        let image = render_lane(
            &gpu,
            &mut renderer,
            &scene,
            &ledger,
            options.into(),
            sequence as u64 + 1,
        );
        let mut oracle = Oracle::new(TARGET);
        let instances = paint_depth(&mut oracle, &scene, scale_maximum, transform);
        assert!(instances > 0);
        let label = format!("summary depth ({transform:?}, maximum {scale_maximum})");
        let comparison = oracle.assert_matches(&image, &label);
        report(&label, &gpu, instances, &comparison);
    }
}

/// The summary pipeline takes its colours from the same palette uniform: a theme switch
/// recolours resident depth pages with no page upload.
#[test]
fn switching_palette_recolours_resident_summary_pages_without_any_geometry_upload() {
    use crate::palette::{AlignmentPalette, PaletteToken, ThemeId};
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer = AlignmentSummaryGpuRenderer::new(&gpu.device, FORMAT);
    let mut options = lane_options(false);
    options.depth.scale_maximum = Some(2);
    options.depth.transform = CoverageScaleTransform::Linear;
    let render = |renderer: &mut AlignmentSummaryGpuRenderer, sequence: u64| {
        let requests = [AlignmentSummaryGpuSceneRequest {
            scene: &scene,
            coherence_scope: 1,
            lane_identity: 1,
            camera_sequence: sequence,
            camera: camera(),
            screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
            scissor: SCISSOR,
            options: options.into(),
            overlays: AlignmentSummaryFrameOverlays::default(),
            junction_presentation: None,
        }];
        let (frame, stats, _) = renderer
            .prepare_with_admission(&gpu.device, &gpu.queue, &requests, &ledger, sequence)
            .unwrap();
        (
            render_image(&gpu, TARGET, |pass| renderer.render(pass, &frame)),
            stats,
        )
    };
    let (light_image, light_stats) = render(&mut renderer, 1);
    assert!(light_stats.geometry_uploaded_bytes > 0);
    let mut previous = light_image.clone();
    for (index, theme) in [ThemeId::Dark, ThemeId::Light].into_iter().enumerate() {
        let palette = AlignmentPalette::for_theme(theme);
        assert!(renderer.set_palette(&gpu.queue, &palette));
        assert_eq!(renderer.palette_uploads(), index as u64 + 1);
        let (image, stats) = render(&mut renderer, index as u64 + 2);
        assert_eq!(stats.geometry_uploaded_bytes, 0, "{theme:?}");
        assert_eq!(stats.page_cache_misses, 0, "{theme:?}");
        assert_eq!(stats.resident_gpu_bytes, light_stats.resident_gpu_bytes);
        assert_ne!(image, previous, "{theme:?} did not change any pixel");
        let mut oracle = Oracle::new(TARGET);
        let mut painted = 0;
        for page in pages(&scene, SummaryPageFamily::Depth) {
            let PackedAlignmentSummaryPage::Depth { instances, .. } =
                PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::depth(
                    page,
                ))
                .unwrap()
            else {
                unreachable!();
            };
            for instance in instances {
                if let Some(bounds) = project_alignment_summary_depth_instance(
                    instance,
                    page.interval().start(),
                    camera(),
                    2,
                    CoverageScaleTransform::Linear,
                ) {
                    paint_solid(
                        &mut oracle,
                        quad(bounds),
                        palette.color(PaletteToken::SUMMARY_DEPTH),
                    );
                    painted += 1;
                }
            }
        }
        assert!(painted > 0);
        let label = format!("summary depth under the {theme:?} palette");
        let comparison = oracle.assert_matches(&image, &label);
        report(&label, &gpu, painted, &comparison);
        previous = image;
    }
    assert_eq!(previous, light_image);
}

#[test]
fn summary_allele_pixels_match_the_cpu_oracle_over_depth() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    // An all-`A` reference: every non-A read base is an alternate allele.
    let scene = test_snapshot_with_reference(&ledger, vec![b'A'; 100]);
    let mut renderer = AlignmentSummaryGpuRenderer::new(&gpu.device, FORMAT);
    let scale_maximum = 2;
    let mut options = lane_options(true);
    options.depth.scale_maximum = Some(scale_maximum);
    options.alleles.as_mut().unwrap().scale_maximum = Some(scale_maximum);
    let image = render_lane(&gpu, &mut renderer, &scene, &ledger, options.into(), 1);

    let mut oracle = Oracle::new(TARGET);
    // Depth is drawn first, alleles over it.
    paint_depth(
        &mut oracle,
        &scene,
        scale_maximum,
        CoverageScaleTransform::Linear,
    );
    let depth_pages = pages(&scene, SummaryPageFamily::Depth);
    let mut alleles = 0;
    for page in pages(&scene, SummaryPageFamily::Alleles) {
        let depth = depth_pages
            .iter()
            .find(|depth| depth.key().tile() == page.key().tile())
            .unwrap();
        let PackedAlignmentSummaryPage::Alleles { instances } =
            PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::alleles(
                page, depth,
            ))
            .unwrap()
        else {
            unreachable!();
        };
        for instance in instances {
            if let Some(projected) = project_alignment_summary_allele_instance(
                instance,
                page.interval().start(),
                camera(),
                scale_maximum,
                CoverageScaleTransform::Linear,
                0.0,
                false,
            ) {
                paint_solid(
                    &mut oracle,
                    quad(projected.bounds),
                    BASE_COLORS[projected.allele as usize],
                );
                alleles += 1;
            }
        }
    }
    assert!(
        alleles >= 3,
        "the fixture must show several alternate alleles"
    );
    let comparison = oracle.assert_matches(&image, "summary alleles");
    report("summary alleles", &gpu, alleles, &comparison);
}

#[test]
fn summary_modification_pixels_match_the_cpu_oracle_by_type_and_against_unmodified() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer = AlignmentSummaryGpuRenderer::new(&gpu.device, FORMAT);
    for (sequence, (probability_weighted, color_mode)) in [
        (false, ModifiedBaseColorMode::ByType),
        (true, ModifiedBaseColorMode::Mono),
        (false, ModifiedBaseColorMode::ModifiedVsUnmodified),
    ]
    .into_iter()
    .enumerate()
    {
        let options = modification_options(probability_weighted, Vec::new(), color_mode);
        let image = render_lane(
            &gpu,
            &mut renderer,
            &scene,
            &ledger,
            options.into(),
            sequence as u64 + 1,
        );
        let mut oracle = Oracle::new(TARGET);
        let mut painted = 0;
        for page in pages(&scene, SummaryPageFamily::Modifications) {
            let PackedAlignmentSummaryPage::Modifications {
                instances,
                metadata,
            } = PackedAlignmentSummaryPage::from_request(
                AlignmentSummaryGpuPageRequest::modifications(page),
            )
            .unwrap()
            else {
                unreachable!();
            };
            let page_start = page.interval().start();
            if color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified {
                // Background bands (shape 1) fill the lane height wherever any series exists.
                // No public oracle projects them; a full-scale depth instance over the same
                // span has exactly the band's rectangle.
                for index in metadata.background_range() {
                    let instance = instances[index as usize];
                    assert_eq!(instance.shape, 1);
                    if instance.later_code_mask == 0 {
                        continue;
                    }
                    let whole =
                        |words: [f32; 2]| (f64::from(words[0]) + f64::from(words[1])) as u64;
                    let band = AlignmentSummaryGpuDepthInstance::new(
                        page.key(),
                        whole(instance.start_offset),
                        whole(instance.end_offset),
                        1,
                    )
                    .unwrap();
                    let bounds = project_alignment_summary_depth_instance(
                        band,
                        page_start,
                        camera(),
                        1,
                        CoverageScaleTransform::Linear,
                    )
                    .unwrap();
                    paint_solid(
                        &mut oracle,
                        quad(bounds),
                        crate::palette::AlignmentPalette::light()
                            .color(crate::palette::PaletteToken::SUMMARY_MODIFICATION_BACKGROUND),
                    );
                    painted += 1;
                }
            }
            let foreground = if probability_weighted {
                metadata.weighted_range()
            } else {
                metadata.raw_range()
            };
            for index in foreground {
                let instance = instances[index as usize];
                if let Some(projected) = project_alignment_summary_modification_instance(
                    instance,
                    page_start,
                    camera(),
                    probability_weighted,
                    0,
                ) {
                    let colour = if color_mode == ModifiedBaseColorMode::ByType {
                        crate::palette::AlignmentPalette::light().resolve(
                            crate::palette::PaletteColor::decode(
                                u8::try_from(instance.by_type_token).unwrap(),
                                0,
                            ),
                        )
                    } else {
                        crate::palette::AlignmentPalette::light()
                            .color(crate::palette::PaletteToken::SUMMARY_MODIFICATION)
                    };
                    paint_solid(&mut oracle, quad(projected.bounds), colour);
                    painted += 1;
                }
            }
        }
        assert!(painted > 0);
        let label =
            format!("summary modifications ({color_mode:?}, weighted {probability_weighted})");
        let comparison = oracle.assert_matches(&image, &label);
        report(&label, &gpu, painted, &comparison);
    }
}

/// Mirror of the junction branch of `alignment_summary.wgsl`'s fragment stage. `point` is
/// relative to the top-left of the arc's full (unclamped) bounds, in pixels.
fn junction_alpha(
    point: [f64; 2],
    full_width: f64,
    height: f64,
    stroke: f64,
    component: AlignmentSummaryJunctionComponentToken,
) -> f64 {
    let downward = component == AlignmentSummaryJunctionComponentToken::Negative;
    let inner_height = (height - stroke).max(0.0);
    let curve = |parameter: f64| {
        let offset = stroke * 0.5 + inner_height * 4.0 * (parameter - 0.5) * (parameter - 0.5);
        if downward { height - offset } else { offset }
    };
    let parameter = point[0] / full_width;
    let slope = 8.0 * inner_height * (parameter - 0.5) / full_width.max(1.0);
    let distance = (point[1] - curve(parameter)).abs() / (1.0 + slope * slope).sqrt();
    let mut alpha = 1.0 - smoothstep(stroke * 0.5, stroke * 0.5 + 1.0, distance);
    if full_width >= 12.0 && component != AlignmentSummaryJunctionComponentToken::Unknown {
        let marker_fraction = if downward { 0.30 } else { 0.70 };
        let delta = [
            point[0] - full_width * marker_fraction,
            point[1] - curve(marker_fraction),
        ];
        if delta[0].abs() <= 3.0 && delta[1].abs() <= 3.0 {
            let directed_x = if downward { -delta[0] } else { delta[0] };
            let chevron_distance = (delta[1].abs() - 0.5 * (2.0 - directed_x)).abs();
            alpha = alpha.max(1.0 - smoothstep(0.65, 1.45, chevron_distance));
        }
    }
    alpha
}

/// Paints one projected junction arc: the quad is the part of the arc inside the lane, while
/// the curve is parameterised over the junction's full span.
fn paint_junction_arc(
    oracle: &mut Oracle,
    projected: &crate::AlignmentSummaryGpuProjectedJunction,
) {
    let colour: [f64; 4] = match projected.component {
        AlignmentSummaryJunctionComponentToken::Positive => [0.84, 0.30, 0.38, 0.82],
        AlignmentSummaryJunctionComponentToken::Negative => [0.25, 0.45, 0.82, 0.82],
        _ => [0.42, 0.45, 0.50, 0.76],
    };
    let full = quad(projected.bounds);
    let left = full.x.max(CLIP.x);
    let right = (full.x + full.width)
        .min(CLIP.x + CLIP.width)
        .max(left + 1.0);
    let visible = Quad {
        x: left,
        y: full.y,
        width: right - left,
        height: full.height,
    };
    let alpha = |x: f64, y: f64| {
        junction_alpha(
            [x - full.x, y - full.y],
            full.width,
            full.height,
            projected.stroke_width,
            projected.component,
        )
    };
    oracle.paint(visible, scissor(), |x, y| match visible.classify(x, y) {
        Some(false) => Shade::Outside,
        None => Shade::Uncertain,
        Some(true) => {
            let centre = alpha(x, y);
            // The chevron marker's bounding box is a hard edge inside the quad.
            let jumps = [
                (-EDGE_BAND, 0.0),
                (EDGE_BAND, 0.0),
                (0.0, -EDGE_BAND),
                (0.0, EDGE_BAND),
            ]
            .into_iter()
            .any(|(dx, dy)| (alpha(x + dx, y + dy) - centre).abs() > 0.1);
            if jumps {
                Shade::Uncertain
            } else if centre <= 0.0 {
                Shade::Outside
            } else {
                Shade::Colour([colour[0], colour[1], colour[2], colour[3] * centre])
            }
        }
    });
}

#[test]
fn summary_junction_arc_pixels_match_the_cpu_oracle_collapsed_and_expanded() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer = AlignmentSummaryGpuRenderer::new(&gpu.device, FORMAT);
    let scale_maximum = 2.0;
    for (sequence, display_mode) in [
        JunctionDisplayMode::Collapsed,
        JunctionDisplayMode::Expanded,
    ]
    .into_iter()
    .enumerate()
    {
        let mut options = sashimi_options(
            display_mode,
            AlignmentSashimiScalePolicy::Fixed(scale_maximum),
        );
        options.projection.scale_maximum = Some(scale_maximum);
        options.projection.show_labels = false;
        let image = render_lane(
            &gpu,
            &mut renderer,
            &scene,
            &ledger,
            options.into(),
            sequence as u64 + 1,
        );
        let row_count = scene.summaries().expanded_junction_row_count();
        let mut oracle = Oracle::new(TARGET);
        let mut arcs = 0;
        for page in pages(&scene, SummaryPageFamily::Junctions) {
            let PackedAlignmentSummaryPage::Junctions { instances, .. } =
                PackedAlignmentSummaryPage::from_request(
                    AlignmentSummaryGpuPageRequest::junctions(page),
                )
                .unwrap()
            else {
                unreachable!();
            };
            for instance in instances {
                let Some(projected) = project_alignment_summary_junction_instance_with_labels(
                    instance,
                    page.interval().start(),
                    camera(),
                    scale_maximum,
                    1.0,
                    row_count,
                    options.projection.minimum_depth,
                    options.projection.minimum_flank,
                    display_mode,
                    options.projection.show_labels,
                ) else {
                    continue;
                };
                assert_ne!(
                    projected.component,
                    AlignmentSummaryJunctionComponentToken::Annotation,
                    "the fixture carries no junction annotation"
                );
                paint_junction_arc(&mut oracle, &projected);
                arcs += 1;
            }
        }
        assert!(arcs > 0);
        // Row baselines are frame overlays drawn after every page.
        let rows = match display_mode {
            JunctionDisplayMode::Collapsed => 1,
            JunctionDisplayMode::Expanded => row_count.max(1),
        };
        let band_height = CLIP.height / f64::from(rows);
        for row in 0..rows {
            paint_solid(
                &mut oracle,
                Quad {
                    x: f64::from(CLIP.x as f32),
                    y: f64::from((CLIP.y + (f64::from(row) + 0.5) * band_height) as f32),
                    width: CLIP.width,
                    height: 1.0,
                },
                JUNCTION_BASELINE_COLOUR,
            );
        }
        let label = format!("summary junction arcs ({display_mode:?})");
        let comparison = oracle.assert_matches(&image, &label);
        report(&label, &gpu, arcs, &comparison);
    }
}
