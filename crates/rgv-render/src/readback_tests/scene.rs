use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentCamera, AlignmentInteractionState, AlignmentPresentationScene, AllocationLedger,
    CpuClipRect,
};
use rgv_core::{ContigId, GenomicInterval, Viewport};
use rgv_model::{CigarOp, CigarOpKind};

use super::{
    FORMAT, Oracle, Quad, Shade, TestGpu, edge_is_decidable, first_column_at_or_after,
    painted_columns, render_image, shade_with_probe, test_gpu,
};
use crate::{
    AlignmentGpuInstance, AlignmentGpuRenderer, AlignmentGpuSceneRequest, AlignmentGpuScissor,
    AlignmentGpuShape, PackedAlignmentPage,
    alignment_scene::tests::{
        TestRead, legacy_semantic_alignment_color, test_scene_colored, test_scene_over,
    },
    palette::{AlignmentPalette, ThemeId},
    project_alignment_gpu_instance,
};

const TARGET: [u32; 2] = [300, 120];
/// The lane rectangle is deliberately off the pixel grid: with integer origins and these row
/// heights every row edge would land exactly on a pixel centre, where coverage is ambiguous.
const CLIP: CpuClipRect = CpuClipRect {
    x: 16.4,
    y: 8.3,
    width: 256.0,
    height: 96.0,
};
const SCISSOR: AlignmentGpuScissor = AlignmentGpuScissor {
    x: 16,
    y: 8,
    width: 256,
    height: 96,
};

fn cigar(operations: &[(CigarOpKind, u32)]) -> Vec<CigarOp> {
    operations
        .iter()
        .map(|(kind, len)| CigarOp {
            kind: *kind,
            len: *len,
        })
        .collect()
}

/// Reads placed relative to `origin`: both strands, a deletion (event line) and an insertion
/// (point marker), overlapping so that several rows are occupied.
fn fixture_reads(origin: u64) -> Vec<TestRead> {
    vec![
        TestRead::forward(origin + 1),
        TestRead {
            start: origin + 3,
            reverse: true,
            cigar: cigar(&[(CigarOpKind::Match, 10)]),
        },
        TestRead {
            start: origin + 2,
            reverse: false,
            cigar: cigar(&[
                (CigarOpKind::Match, 4),
                (CigarOpKind::Deletion, 3),
                (CigarOpKind::Match, 6),
            ]),
        },
        TestRead {
            start: origin + 5,
            reverse: true,
            cigar: cigar(&[
                (CigarOpKind::Match, 5),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 3),
            ]),
        },
    ]
}

/// The quad the vertex stage emits for one instance, before the scissor: the same arithmetic as
/// [`project_alignment_gpu_instance`] without its clamp to the camera clip, which the fragment
/// shape rules need because they are evaluated in unclipped local coordinates.
fn unclipped_quad(
    instance: AlignmentGpuInstance,
    page_start: u64,
    page_row_start: u32,
    camera: AlignmentCamera,
) -> Quad {
    let project = |offset: u64| {
        camera.clip().x
            + (page_start as f64 + offset as f64 - camera.exact_start()) / camera.bases_per_pixel()
    };
    let mut left = project(instance.start_offset());
    let mut right = project(instance.end_offset());
    let row = f64::from(page_row_start + instance.row_offset());
    let row_top = camera.clip().y
        + (row - camera.row_scroll()) * camera.row_height()
        + (camera.row_height() - camera.read_height()) * 0.5;
    let (mut top, mut height) = (row_top, camera.read_height().max(1.0));
    if instance.shape() == AlignmentGpuShape::EventLine as u32 {
        top = row_top + camera.read_height() * 0.45;
        height = (camera.read_height() * 0.10).max(1.0);
    }
    if instance.shape() == AlignmentGpuShape::PointMarker as u32 {
        let width = (right - left).max(1.5);
        left -= (width - (right - left)) * 0.5;
        right = left + width;
    } else {
        right = right.max(left + 1.0);
    }
    Quad {
        x: left,
        y: top,
        width: right - left,
        height,
    }
}

/// Mirror of `alignment_scene.wgsl`'s fragment stage: only direction terminals are shaped.
fn fragment_is_kept(shape: u32, local: [f64; 2], size: [f64; 2]) -> bool {
    if shape != AlignmentGpuShape::DirectionTerminalRight as u32
        && shape != AlignmentGpuShape::DirectionTerminalLeft as u32
    {
        return true;
    }
    let width = size[0].max(0.0001);
    let distance_from_tip = if shape == AlignmentGpuShape::DirectionTerminalLeft as u32 {
        local[0]
    } else {
        size[0] - local[0]
    };
    let head_width = (size[1] * 0.55).min(width * 0.35);
    if distance_from_tip < head_width {
        let half_height = size[1] * 0.5;
        let allowed = half_height * (distance_from_tip / head_width).clamp(0.0, 1.0);
        return (local[1] - half_height).abs() <= allowed;
    }
    true
}

struct ExpectedInstance {
    quad: Quad,
    shape: u32,
}

/// Builds the oracle image for `scene` under `camera` and returns the projected instances.
fn oracle_for(
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
) -> (Oracle, Vec<ExpectedInstance>) {
    oracle_with_palette(scene, camera, None)
}

/// `palette` of `None` is the golden path: colours come from the legacy baked-colour rules, not
/// from any palette, so the Light theme is checked against what RGV drew before palettes.
fn oracle_with_palette(
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
    palette: Option<&AlignmentPalette>,
) -> (Oracle, Vec<ExpectedInstance>) {
    let scissor = [SCISSOR.x, SCISSOR.y, SCISSOR.width, SCISSOR.height];
    let mut oracle = Oracle::new(TARGET);
    let mut expected = Vec::new();
    for page in scene.visible_pages(camera.interval(), camera.visible_rows()) {
        let Ok(packed) = PackedAlignmentPage::from_semantic(page) else {
            continue;
        };
        for (instance, semantic_index) in packed.instances.into_iter().zip(packed.semantic_indices)
        {
            let page_start = page.interval().start();
            let page_row_start = page.row_range().start;
            let quad = unclipped_quad(instance, page_start, page_row_start, camera);
            // Tie the unclipped mirror to the crate's public oracle: clipped to the camera
            // clip, the two must describe the same rectangle.
            let clip = camera.clip();
            let clipped_left = quad.x.max(clip.x);
            let clipped_top = quad.y.max(clip.y);
            let clipped_right = (quad.x + quad.width).min(clip.x + clip.width);
            let clipped_bottom = (quad.y + quad.height).min(clip.y + clip.height);
            match project_alignment_gpu_instance(instance, page_start, page_row_start, camera) {
                Some(bounds) => {
                    assert!((bounds.x - clipped_left).abs() < 1.0e-9);
                    assert!((bounds.y - clipped_top).abs() < 1.0e-9);
                    assert!((bounds.x + bounds.width - clipped_right).abs() < 1.0e-9);
                    assert!((bounds.y + bounds.height - clipped_bottom).abs() < 1.0e-9);
                }
                None => assert!(clipped_right <= clipped_left || clipped_bottom <= clipped_top),
            }
            let colour = if let Some(palette) = palette {
                palette.resolve(instance.palette_color())
            } else {
                let primitive = page.primitives()[semantic_index as usize];
                legacy_semantic_alignment_color(primitive.component, primitive.style)
            }
            .map(f64::from);
            let shape = instance.shape();
            oracle.paint(quad, scissor, |x, y| match quad.classify(x, y) {
                Some(false) => Shade::Outside,
                None => Shade::Uncertain,
                Some(true) => shade_with_probe(x, y, |x, y| {
                    fragment_is_kept(shape, [x - quad.x, y - quad.y], [quad.width, quad.height])
                        .then_some(colour)
                }),
            });
            expected.push(ExpectedInstance { quad, shape });
        }
    }
    (oracle, expected)
}

fn render_scene(
    gpu: &TestGpu,
    renderer: &mut AlignmentGpuRenderer,
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
    ledger: &AllocationLedger,
    sequence: u64,
) -> image::RgbaImage {
    let interaction = AlignmentInteractionState::default();
    let requests = [AlignmentGpuSceneRequest {
        scene,
        camera_sequence: sequence,
        camera,
        screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
        scissor: SCISSOR,
        interaction: &interaction,
    }];
    let (frame, _, _) = renderer
        .prepare_with_admission(&gpu.device, &gpu.queue, &requests, ledger, sequence)
        .unwrap();
    assert!(frame.draw_count() > 0);
    render_image(gpu, TARGET, |pass| renderer.render(pass, &frame))
}

#[test]
fn alignment_scene_pixels_match_the_cpu_oracle() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
    let scene = test_scene_over(
        &ledger,
        interval,
        100,
        // Several tiles, so reads cross page boundaries and each page has its own origin.
        NonZeroU64::new(8).unwrap(),
        &fixture_reads(4),
        NonZeroU32::new(2).unwrap(),
    );
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let cameras = [
        // 16 points per base: every component is several pixels wide.
        AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 2, 18).unwrap(),
            0.0,
            14.0,
            11.0,
            CLIP,
        )
        .unwrap(),
        // Scrolled half a row, with a fractional genomic origin.
        AlignmentCamera::from_viewport(
            Viewport::new(
                GenomicInterval::new(ContigId::new(0), 3, 16).unwrap(),
                256.0,
            )
            .unwrap()
            .pan_pixels(5.37, 100)
            .unwrap(),
            0.5,
            18.0,
            13.0,
            CLIP,
        )
        .unwrap(),
        // The whole prepared interval: bases are narrower than three pixels.
        AlignmentCamera::new(interval, 0.0, 9.0, 7.0, CLIP).unwrap(),
    ];
    for (index, camera) in cameras.into_iter().enumerate() {
        let image = render_scene(
            &gpu,
            &mut renderer,
            &scene,
            camera,
            &ledger,
            index as u64 + 1,
        );
        let (oracle, expected) = oracle_for(&scene, camera);
        assert!(
            expected
                .iter()
                .any(|instance| instance.shape == AlignmentGpuShape::DirectionTerminalLeft as u32)
        );
        let comparison = oracle.assert_matches(&image, &format!("alignment scene camera {index}"));
        eprintln!(
            "alignment scene camera {index} on {}: {} instances, {} pixels compared, {} painted, \
             {} boundary pixels excluded",
            gpu.description,
            expected.len(),
            comparison.compared,
            comparison.painted,
            comparison.uncertain
        );
    }
}

/// REND-10 / PROD-17. Recolouring is a palette uniform write: with the pages already resident,
/// switching theme changes the pixels while no page byte is uploaded again, and under every
/// palette the GPU resolves each instance's token exactly as `AlignmentPalette::resolve` does.
#[test]
fn switching_palette_recolours_resident_pages_without_any_geometry_upload() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
    let scene = test_scene_over(
        &ledger,
        interval,
        100,
        NonZeroU64::new(8).unwrap(),
        &fixture_reads(4),
        NonZeroU32::new(2).unwrap(),
    );
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 2, 18).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    let interaction = AlignmentInteractionState::default();
    let render = |renderer: &mut AlignmentGpuRenderer, sequence: u64| {
        let requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: sequence,
            camera,
            screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
            scissor: SCISSOR,
            interaction: &interaction,
        }];
        let (frame, stats, admission) = renderer
            .prepare_with_admission(&gpu.device, &gpu.queue, &requests, &ledger, sequence)
            .unwrap();
        let image = render_image(&gpu, TARGET, |pass| renderer.render(pass, &frame));
        (image, stats, admission)
    };

    assert_eq!(*renderer.palette(), AlignmentPalette::light());
    let (light_image, first_stats, first_admission) = render(&mut renderer, 1);
    assert!(first_admission.uploaded_bytes > 0);
    // Sixteen bytes per instance, charged exactly (REND-8); the same pages were forty bytes per
    // instance before colours became palette indices.
    assert_eq!(
        first_admission.uploaded_bytes,
        first_admission.instances * size_of::<AlignmentGpuInstance>()
    );
    eprintln!(
        "{} pages, {} instances: {} bytes resident (was {} at forty bytes per instance)",
        first_admission.admitted_pages,
        first_admission.instances,
        first_admission.uploaded_bytes,
        first_admission.instances * 40
    );
    assert_eq!(renderer.palette_uploads(), 0);
    let resident_bytes = first_stats.resident_gpu_bytes;
    let charged = ledger.snapshot().unwrap().charged;
    // The default palette is Light, and Light is the legacy colours.
    oracle_for(&scene, camera)
        .0
        .assert_matches(&light_image, "light palette against the legacy goldens");
    // Setting the palette already in force uploads nothing at all.
    assert!(!renderer.set_palette(&gpu.queue, &AlignmentPalette::light()));
    assert_eq!(renderer.palette_uploads(), 0);

    let mut previous = light_image.clone();
    for (index, theme) in [
        ThemeId::Dark,
        ThemeId::LightColorBlind,
        ThemeId::DarkColorBlind,
        ThemeId::Light,
    ]
    .into_iter()
    .enumerate()
    {
        let palette = AlignmentPalette::for_theme(theme);
        assert!(renderer.set_palette(&gpu.queue, &palette));
        assert_eq!(renderer.palette_uploads(), index as u64 + 1);
        let (image, stats, admission) = render(&mut renderer, index as u64 + 2);
        // No page was admitted, re-packed or re-uploaded; residency and the ledger are as before.
        assert_eq!(admission.uploaded_bytes, 0, "{theme:?}");
        assert_eq!(admission.admitted_pages, 0, "{theme:?}");
        assert_eq!(stats.geometry_uploaded_bytes, 0, "{theme:?}");
        assert_eq!(stats.page_cache_misses, 0, "{theme:?}");
        assert_eq!(stats.resident_gpu_bytes, resident_bytes, "{theme:?}");
        assert_eq!(ledger.snapshot().unwrap().charged, charged, "{theme:?}");
        assert_ne!(image, previous, "{theme:?} did not change any pixel");
        let (oracle, expected) = oracle_with_palette(&scene, camera, Some(&palette));
        assert!(!expected.is_empty());
        let comparison = oracle.assert_matches(&image, &format!("{theme:?} palette"));
        eprintln!(
            "{theme:?} palette on {}: {} pixels compared, {} painted, 0 geometry bytes uploaded",
            gpu.description, comparison.compared, comparison.painted
        );
        previous = image;
    }
    // Back on Light the frame is pixel-identical to the first one.
    assert_eq!(previous, light_image);
}

/// The two non-table colour forms on the GPU: the mapping-quality ramp (a position between two
/// palette slots, carried as f32 bits) and the inline RGBA8 escape hatch used by hashed
/// categories. Under Light both must reproduce the legacy baked colours; under Dark the ramp
/// follows the palette while inline colours are, by design, unchanged.
#[test]
fn ramp_and_inline_colours_match_the_cpu_oracle_under_light_and_dark() {
    use crate::palette::PaletteColor;
    use rgv_core::AlignmentColorMode;
    let Some(gpu) = test_gpu() else {
        return;
    };
    let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 2, 18).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    for mode in [
        AlignmentColorMode::MappingQuality,
        AlignmentColorMode::MateContig,
    ] {
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_scene_colored(
            &ledger,
            interval,
            100,
            NonZeroU64::new(8).unwrap(),
            &fixture_reads(4),
            NonZeroU32::new(2).unwrap(),
            Some(mode),
        );
        // The fixture really exercises the form under test, with several distinct values.
        let mut forms = std::collections::BTreeSet::new();
        for page in scene.pages() {
            let Ok(packed) = PackedAlignmentPage::from_semantic(page) else {
                continue;
            };
            for instance in packed.instances {
                match (mode, instance.palette_color()) {
                    (AlignmentColorMode::MappingQuality, PaletteColor::Ramp { position, .. }) => {
                        forms.insert(position.to_bits());
                    }
                    (AlignmentColorMode::MateContig, PaletteColor::Inline(rgba)) => {
                        forms.insert(u32::from_le_bytes(rgba));
                    }
                    _ => {}
                }
            }
        }
        assert!(forms.len() >= 3, "{mode:?} produced only {forms:?}");

        let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
        let light = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 1);
        oracle_for(&scene, camera)
            .0
            .assert_matches(&light, &format!("{mode:?} against the legacy goldens"));
        let dark_palette = AlignmentPalette::dark();
        renderer.set_palette(&gpu.queue, &dark_palette);
        let dark = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 2);
        let comparison = oracle_with_palette(&scene, camera, Some(&dark_palette))
            .0
            .assert_matches(&dark, &format!("{mode:?} under the dark palette"));
        assert_ne!(light, dark);
        eprintln!(
            "{mode:?} colours on {}: {} distinct values, {} pixels compared",
            gpu.description,
            forms.len(),
            comparison.compared
        );
    }
}

/// Sub-pixel check on the read bodies themselves: returns how many blunt read ends were
/// decidable, after asserting each is painted from exactly the column the f64 oracle predicts.
fn blunt_edges_agreeing_with_the_oracle(
    image: &image::RgbaImage,
    expected: &[ExpectedInstance],
    camera: AlignmentCamera,
    step: u32,
) -> usize {
    let mut edges = 0_usize;
    // Sub-pixel check on the read bodies themselves. Each fixture read owns its row.
    let clip_right = CLIP.x + CLIP.width;
    for row in 0..4_u32 {
        let instances = expected
            .iter()
            .filter(|instance| {
                let centre = instance.quad.y + instance.quad.height * 0.5;
                ((centre - CLIP.y) / camera.row_height()).floor() as u32 == row
            })
            .collect::<Vec<_>>();
        assert!(!instances.is_empty(), "row {row} has no instance");
        let left = instances
            .iter()
            .map(|instance| instance.quad.x)
            .fold(f64::INFINITY, f64::min)
            .max(CLIP.x);
        let right = instances
            .iter()
            .map(|instance| instance.quad.x + instance.quad.width)
            .fold(f64::NEG_INFINITY, f64::max)
            .min(clip_right);
        let scanline = (CLIP.y + (f64::from(row) + 0.5) * camera.row_height()) as u32;
        let (first, end) = painted_columns(image, scanline)
            .unwrap_or_else(|| panic!("row {row} is empty at step {step}"));
        // The scanline is half a pixel off the row centre, so it cuts the arrow tip
        // short; only the blunt end of each read is a vertical edge.
        let tip_is_right = instances
            .iter()
            .any(|instance| instance.shape == AlignmentGpuShape::DirectionTerminalRight as u32);
        let tip_is_left = instances
            .iter()
            .any(|instance| instance.shape == AlignmentGpuShape::DirectionTerminalLeft as u32);
        assert!(tip_is_left != tip_is_right, "row {row} must hold one read");
        if tip_is_right && edge_is_decidable(left) {
            assert_eq!(
                i64::from(first),
                first_column_at_or_after(left),
                "step {step} row {row}: left edge at {left}"
            );
            edges += 1;
        }
        if tip_is_left && edge_is_decidable(right) {
            assert_eq!(
                i64::from(end),
                first_column_at_or_after(right),
                "step {step} row {row}: right edge at {right}"
            );
            edges += 1;
        }
    }
    edges
}

/// Sweeps a ~20 px/base camera in 1/64-pixel steps over reads that sit `origin` bases from the
/// start of the single page carrying them, and requires the painted pixels to be exactly the
/// ones the f64 oracle predicts at every step. Every edge crosses pixel centres during the
/// sweep, which bounds any disagreement to well under a tenth of a pixel.
fn sweep_far_from_the_page_start(origin: u64, contig: u64, label: &str) {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    let scene = test_scene_over(
        &ledger,
        GenomicInterval::new(ContigId::new(0), 0, contig).unwrap(),
        contig,
        // One page spans the whole contig, so its start is `origin` bases left of the camera.
        NonZeroU64::new(contig).unwrap(),
        &fixture_reads(origin),
        NonZeroU32::new(16).unwrap(),
    );
    let page_starts = scene
        .pages()
        .iter()
        .map(|page| page.interval().start())
        .collect::<Vec<_>>();
    assert!(
        page_starts.iter().all(|start| *start == 0),
        "{page_starts:?}"
    );
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let base = Viewport::new(
        GenomicInterval::new(ContigId::new(0), origin, origin + 13).unwrap(),
        CLIP.width,
    )
    .unwrap();
    let pixels_per_base = 1.0 / base.bases_per_pixel();
    assert!((19.0..21.0).contains(&pixels_per_base));

    let (mut edges_checked, mut worst_band_excluded) = (0_usize, 0_usize);
    for step in 0..64_u32 {
        let pan = 3.0 + f64::from(step) / 64.0;
        let camera = AlignmentCamera::from_viewport(
            base.pan_pixels(pan, contig).unwrap(),
            0.0,
            14.0,
            11.0,
            CLIP,
        )
        .unwrap();
        assert!(camera.exact_start() > origin as f64 && camera.exact_start().fract() != 0.0);
        let image = render_scene(
            &gpu,
            &mut renderer,
            &scene,
            camera,
            &ledger,
            u64::from(step) + 1,
        );
        let (oracle, expected) = oracle_for(&scene, camera);
        let comparison = oracle.assert_matches(&image, &format!("{label} sweep step {step}"));
        worst_band_excluded = worst_band_excluded.max(comparison.uncertain);

        edges_checked += blunt_edges_agreeing_with_the_oracle(&image, &expected, camera, step);
    }
    assert!(
        edges_checked > 200,
        "only {edges_checked} edges were decidable"
    );
    eprintln!(
        "{label} projection sweep on {}: {edges_checked} sub-pixel edge positions agreed with \
         the f64 oracle ({pixels_per_base:.2} px/base, at most {worst_band_excluded} boundary \
         pixels excluded per frame)",
        gpu.description
    );
}

/// REND-7. The camera sits one megabase from the start of the page that carries the reads, at
/// about twenty pixels per base, so page-local offsets and the camera origin are both ~1e6 and
/// their difference must survive to a small fraction of a base. With the original double-single
/// f32 arithmetic this test failed on Metal: fast-math reassociated the sums, the low words were
/// lost, and edges followed the camera in steps of `ulp(1e6) = 1/16` base (1.2 pixels) instead
/// of continuously. `project_genomic_x` now cancels in integers.
#[test]
fn double_single_projection_agrees_with_the_f64_oracle_one_megabase_from_the_page_start() {
    sweep_far_from_the_page_start(1_000_000, 1 << 21, "megabase");
}

/// The same sweep past 2^24 bases, where a page-local offset no longer fits one f32: the odd
/// origin puts a non-zero remainder in the low word of every instance offset, so this also
/// covers the second integer term of `project_genomic_x`.
#[test]
fn projection_uses_both_offset_words_beyond_single_f32_integer_precision() {
    sweep_far_from_the_page_start(20_000_001, 1 << 25, "twenty-megabase");
}
