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
        BOUNDARY_FIXTURE_ORIGIN, TestRead, boundary_marker_scene, legacy_semantic_alignment_color,
        test_scene_colored, test_scene_over,
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
/// shape rules need because they are evaluated in unclipped local coordinates. Also returns the
/// shape the fragment stage sees: a clip edge drawn as a hairline is a plain rectangle.
fn unclipped_quad(
    instance: AlignmentGpuInstance,
    page_start: u64,
    page_row_start: u32,
    camera: AlignmentCamera,
) -> (Quad, u32) {
    // Written out independently of `boundary_mark_detail`: full marks from one point per base,
    // one-point hairlines from 0.1, nothing (a zero-width quad) below.
    let points_per_base = 1.0 / camera.bases_per_pixel();
    let (full_marks, hidden_marks) = (points_per_base >= 1.0, points_per_base < 0.1);
    let mut fragment_shape = instance.shape();
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
    let at_end = instance.shape() == AlignmentGpuShape::PointMarkerAtEnd as u32;
    if at_end || instance.shape() == AlignmentGpuShape::PointMarker as u32 {
        // Written out independently of `point_marker_extent`: an I-beam whose bar is a quarter
        // of a base wide within [2, 3] points, with serifs reaching 2.5 points beyond it on
        // each side, centred on the start (or end) edge of the base. Zoomed out it is a plain
        // one-point hairline.
        let width = if full_marks {
            ((right - left) * 0.25).clamp(2.0, 3.0) + 5.0
        } else {
            fragment_shape = AlignmentGpuShape::Rectangle as u32;
            1.0
        };
        let boundary = if at_end { right } else { left };
        left = boundary - width * 0.5;
        right = if hidden_marks { left } else { left + width };
    } else if (6..=13).contains(&instance.shape()) {
        // Written out independently of `clip_edge_extent`. Shape bits: 1 end edge, 2 hard clip,
        // 4 behind the arrowhead. A soft clip's edge is half a base wide within [2, 6] points, a
        // hard clip's two; behind an arrowhead it moves inward from the boundary (the read's
        // tip) by the head length, min(0.55 x height, 10 x points per base), or nothing when
        // that is under half a point. Zoomed out either edge is a plain one-point hairline.
        let bits = instance.shape() - 6;
        let base = right - left;
        let width = if !full_marks {
            fragment_shape = AlignmentGpuShape::Rectangle as u32;
            1.0
        } else if bits & 2 == 0 {
            (base * 0.5).clamp(2.0, 6.0)
        } else {
            2.0
        };
        let head = (height * 0.55).min(points_per_base * 10.0);
        let head = if head < 0.5 { 0.0 } else { head };
        let inset = if bits & 4 == 0 { 0.0 } else { head };
        if bits & 1 == 0 {
            left += inset;
            right = left + width;
        } else {
            right -= inset;
            left = right - width;
        }
        if hidden_marks {
            right = left;
        }
    } else if instance.shape() == AlignmentGpuShape::DirectionTerminalRight as u32 {
        // The tip stays on the read's end: the one-point minimum grows leftwards.
        left = left.min(right - 1.0);
    } else if instance.shape() == AlignmentGpuShape::ClipGhostCell as u32
        && !rgv_alignment_scene::clip_ghost_visible(points_per_base)
    {
        // Ghost cells appear only together with their letters.
        right = left;
    } else {
        right = right.max(left + 1.0);
    }
    (
        Quad {
            x: left,
            y: top,
            width: right - left,
            height,
        },
        fragment_shape,
    )
}

/// Mirror of `alignment_scene.wgsl`'s fragment stage: direction terminals are cut to an
/// arrowhead, a soft clip's edge to a zigzag stroke and an insertion to an I-beam; everything
/// else is solid. A direction terminal's tip-side edge is `bases_from_tip` whole bases short of
/// its read's tip.
fn fragment_is_kept(
    shape: u32,
    local: [f64; 2],
    size: [f64; 2],
    points_per_base: f64,
    bases_from_tip: u32,
) -> bool {
    if shape == AlignmentGpuShape::PointMarker as u32
        || shape == AlignmentGpuShape::PointMarkerAtEnd as u32
    {
        // Written out independently of `insertion_mark_covers`: the bar is what is left of the
        // width after two 2.5-point serifs, centred; the serifs are 0.15 of the height thick,
        // within [1, 2] points, at the top and bottom.
        let bar = (size[0] - 5.0).max(0.0);
        let serif = (size[1] * 0.15).clamp(1.0, 2.0);
        return (local[0] - size[0] * 0.5).abs() < bar * 0.5
            || local[1] < serif
            || local[1] > size[1] - serif;
    }
    if (6..=13).contains(&shape) && (shape - 6) & 2 == 0 {
        // Written out independently of `clip_edge_covers`: a 1.5-point stroke zigzagging
        // between the boundary side and the inner side, round(height / 4) teeth (at least one).
        let teeth = (size[1] / 4.0 + 0.5).floor().max(1.0);
        let period = size[1] / teeth;
        let phase = (local[1] / period).rem_euclid(1.0);
        let centre = 0.75 + (size[0] - 1.5).max(0.0) * (1.0 - (2.0 * phase - 1.0).abs());
        let slope = 2.0 * (size[0] - 1.5).max(0.0) / period;
        let from_boundary = if (shape - 6) & 1 == 0 {
            local[0]
        } else {
            size[0] - local[0]
        };
        return (from_boundary - centre).abs() <= 0.75 * (1.0 + slope * slope).sqrt();
    }
    if shape != AlignmentGpuShape::DirectionTerminalRight as u32
        && shape != AlignmentGpuShape::DirectionTerminalLeft as u32
    {
        return true;
    }
    let distance_from_tip = if shape == AlignmentGpuShape::DirectionTerminalLeft as u32 {
        local[0]
    } else {
        size[0] - local[0]
    } + f64::from(bases_from_tip) * points_per_base;
    // The head length depends on the read height and the zoom only, whatever the shape's own
    // width: 0.55 x height, at most ten bases, and no head at all under half a point.
    let head_width = (size[1] * 0.55).min(points_per_base * 10.0);
    let head_width = if head_width < 0.5 { 0.0 } else { head_width };
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
            let (quad, fragment_shape) =
                unclipped_quad(instance, page_start, page_row_start, camera);
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
            };
            let colour = if instance.shape() == AlignmentGpuShape::ClipGhostCell as u32 {
                // Ghost cells: the read colour moved half way to the ghost slot, at its alpha.
                let tint = palette
                    .copied()
                    .unwrap_or_else(AlignmentPalette::light)
                    .color(crate::palette::PaletteToken::CLIP_GHOST);
                [
                    colour[0] + (tint[0] - colour[0]) * 0.5,
                    colour[1] + (tint[1] - colour[1]) * 0.5,
                    colour[2] + (tint[2] - colour[2]) * 0.5,
                    tint[3],
                ]
            } else {
                colour
            }
            .map(f64::from);
            let shape = instance.shape();
            let bases_from_tip = instance.bases_from_tip();
            let points_per_base = 1.0 / camera.bases_per_pixel();
            oracle.paint(quad, scissor, |x, y| match quad.classify(x, y) {
                Some(false) => Shade::Outside,
                None => Shade::Uncertain,
                Some(true) => shade_with_probe(x, y, |x, y| {
                    fragment_is_kept(
                        fragment_shape,
                        [x - quad.x, y - quad.y],
                        [quad.width, quad.height],
                        points_per_base,
                        bases_from_tip,
                    )
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

/// The reported defects on the real GPU: at ~128 points per base an insertion is an I-beam (a
/// three-point bar with serifs across the top and bottom of the read, eight points wide in all)
/// on the boundary between the two reference bases it lies between, never a block the size of a
/// base, and both neighbouring base cells stay read-coloured, exactly where the CPU projector
/// and the f64 oracle put them.
#[test]
#[allow(clippy::too_many_lines)]
fn insertion_i_beam_is_painted_on_the_base_boundary_and_not_over_its_base_cell() {
    use crate::palette::{PaletteColor, PaletteToken};
    use rgv_alignment_scene::{AlignmentComponent, CpuAlignmentProjector};
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    let scene = boundary_marker_scene(&ledger);
    let origin = BOUNDARY_FIXTURE_ORIGIN;
    // Two bases over the 256-point lane: the insertion lies between origin + 21 and origin + 22,
    // whose shared edge is in the middle of the lane.
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), origin + 21, origin + 23).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    let boundary = CLIP.x + 128.0;
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let image = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 1);
    let (oracle, expected) = oracle_for(&scene, camera);
    assert!(
        expected
            .iter()
            .any(|instance| instance.shape == AlignmentGpuShape::PointMarker as u32)
    );
    oracle.assert_matches(&image, "insertion boundary bar");

    let frame = CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
    let marker = frame
        .primitives
        .iter()
        .find(|primitive| matches!(primitive.component, AlignmentComponent::Insertion { .. }))
        .expect("the insertion is projected");
    assert!((marker.x + marker.width * 0.5 - boundary).abs() < 1.0e-9);
    // 128 points per base: the I-beam stays capped at a three-point bar plus two 2.5-point
    // serifs, a sixteenth of the base cell.
    assert!((marker.width - 8.0).abs() < 1.0e-9);

    let palette = AlignmentPalette::light();
    let insertion = palette
        .resolve(PaletteColor::Token(PaletteToken::INSERTION))
        .map(|channel| (f64::from(channel) * 255.0).round() as u8);
    let scanline = (marker.y + marker.height * 0.5) as u32;
    let pixel = |column: u32| {
        let [r, g, b, _] = image.get_pixel(column, scanline).0;
        [r, g, b]
    };
    // Mid-height, only the three-point bar is insertion-coloured: every column whose centre lies
    // inside it, and none of the box's columns beside it.
    let (bar_left, bar_right) = (boundary - 1.5, boundary + 1.5);
    assert!(edge_is_decidable(bar_left) && edge_is_decidable(bar_right));
    let bar_first = first_column_at_or_after(bar_left) as u32;
    let bar_end = first_column_at_or_after(bar_right) as u32;
    assert_eq!(bar_end - bar_first, 3, "{bar_first}..{bar_end}");
    for column in bar_first..bar_end {
        assert_eq!(pixel(column), insertion[..3], "bar column {column}");
    }
    // The serifs: a scanline inside the top serif (0.15 x 11 = 1.65 points thick) and one inside
    // the bottom serif are insertion-coloured across the whole eight-point box.
    let (left, right) = (marker.x, marker.x + marker.width);
    assert!(edge_is_decidable(left) && edge_is_decidable(right));
    let first = first_column_at_or_after(left) as u32;
    let end = first_column_at_or_after(right) as u32;
    assert!(end - first >= 7, "{first}..{end}");
    let serif = 11.0 * 0.15;
    for (name, centre) in [
        ("top", marker.y + serif * 0.5),
        ("bottom", marker.y + marker.height - serif * 0.5),
    ] {
        let row = centre.floor() as u32;
        let row_centre = f64::from(row) + 0.5;
        assert!(
            (row_centre - centre).abs() < serif * 0.5 - 0.05,
            "{name} serif scanline {row} is not inside the serif"
        );
        for column in first..end {
            assert_eq!(
                rgb_at(&image, column, row),
                insertion[..3],
                "{name} serif column {column}"
            );
        }
        assert_ne!(rgb_at(&image, first - 1, row), insertion[..3]);
        assert_ne!(rgb_at(&image, end, row), insertion[..3]);
    }
    let (first, end) = (bar_first, bar_end);
    // The neighbouring base cells, both mid-cell and immediately beside the bar, are the read's
    // own body colour (this fixture's colour mode tints it by strand rather than plain grey):
    // neither base looks substituted.
    let body = frame
        .primitives
        .iter()
        .find(|primitive| {
            primitive.handle == marker.handle
                && matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                )
        })
        .expect("the read body is projected");
    let read = palette
        .resolve(crate::alignment_scene::semantic_alignment_color(
            body.component,
            body.style,
        ))
        .map(|channel| (f64::from(channel) * 255.0).round() as u8);
    let read = [read[0], read[1], read[2]];
    assert_ne!(read, insertion[..3]);
    for column in [
        (CLIP.x + 64.0) as u32,
        first - 2,
        end + 1,
        (CLIP.x + 192.0) as u32,
    ] {
        assert_eq!(pixel(column), read, "base-cell column {column}");
    }
    eprintln!(
        "insertion bar on {}: columns {first}..{end} around boundary {boundary}",
        gpu.description
    );
}

/// 8-bit colour of `token` under the Light palette.
fn light_rgb(token: crate::palette::PaletteToken) -> [u8; 3] {
    let [r, g, b, _] = AlignmentPalette::light()
        .color(token)
        .map(|channel| (f64::from(channel) * 255.0).round() as u8);
    [r, g, b]
}

/// 8-bit body colour of the read `handle` as the CPU projector resolves it under Light.
fn body_rgb(
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
    handle: rgv_alignment_scene::AlignmentHandle,
) -> [u8; 3] {
    let frame = rgv_alignment_scene::CpuAlignmentProjector::project(scene, 1, camera).unwrap();
    let body = frame
        .primitives
        .iter()
        .find(|primitive| {
            primitive.handle == handle
                && matches!(
                    primitive.component,
                    rgv_alignment_scene::AlignmentComponent::AlignedSegment { .. }
                )
        })
        .expect("the read body is projected");
    let [r, g, b, _] = AlignmentPalette::light()
        .resolve(crate::alignment_scene::semantic_alignment_color(
            body.component,
            body.style,
        ))
        .map(|channel| (f64::from(channel) * 255.0).round() as u8);
    [r, g, b]
}

fn rgb_at(image: &image::RgbaImage, column: u32, row: u32) -> [u8; 3] {
    let [r, g, b, _] = image.get_pixel(column, row).0;
    [r, g, b]
}

fn close(left: [u8; 3], right: [u8; 3]) -> bool {
    (0..3).all(|channel| left[channel].abs_diff(right[channel]) <= super::CHANNEL_TOLERANCE)
}

/// A forward read's trailing soft clip on the real GPU at 64 points per base: the dark zigzag
/// sits just inside the arrowhead's base, the arrow tip is still cut to a point on the read's
/// boundary, and the zigzag's columns move from scanline to scanline.
#[test]
fn soft_clip_sawtooth_sits_just_inside_the_arrowhead_on_the_gpu() {
    use crate::palette::PaletteToken;
    use rgv_alignment_scene::AlignmentComponent;
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    let scene = boundary_marker_scene(&ledger);
    let origin = BOUNDARY_FIXTURE_ORIGIN;
    // The `M6 S4` read at origin + 400 ends (with its arrowhead and its clip) at origin + 406,
    // drawn at 16.4 + 3 x 64 = 208.4.
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), origin + 403, origin + 407).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let image = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 1);
    let (oracle, expected) = oracle_for(&scene, camera);
    assert!(
        expected
            .iter()
            .any(|instance| instance.shape == AlignmentGpuShape::SoftClipEdgeEndBehindArrow as u32)
    );
    oracle.assert_matches(&image, "soft clip sawtooth behind the arrowhead");

    let boundary = CLIP.x + 3.0 * 64.0;
    // The arrowhead is 0.55 x 11 = 6.05 points long; the six-point edge ends at its base.
    let (edge_left, edge_right) = (boundary - 6.05 - 6.0, boundary - 6.05);
    let edge = light_rgb(PaletteToken::CLIP_EDGE);
    let white = [255, 255, 255];
    let frame = rgv_alignment_scene::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
    let handle = frame
        .primitives
        .iter()
        .find(|primitive| matches!(primitive.component, AlignmentComponent::SoftClip { .. }))
        .unwrap()
        .handle;
    let body = body_rgb(&scene, camera, handle);
    // Row 0: the read occupies y = 9.8 .. 20.8.
    let mut mean_columns = Vec::new();
    for row in 10..=20_u32 {
        let painted = (0..TARGET[0])
            .filter(|column| close(rgb_at(&image, *column, row), edge))
            .collect::<Vec<_>>();
        assert!(!painted.is_empty(), "scanline {row} has no cut edge");
        for column in &painted {
            let centre = f64::from(*column) + 0.5;
            assert!(
                centre > edge_left - 0.5 && centre < edge_right + 0.5,
                "scanline {row}: edge pixel at {column} outside {edge_left}..{edge_right}"
            );
        }
        // The stroke never fills the edge's whole width: it is a zigzag, not a bar.
        assert!(painted.len() < 6, "scanline {row}: {painted:?}");
        mean_columns
            .push(painted.iter().map(|c| f64::from(*c)).sum::<f64>() / painted.len() as f64);
    }
    let (low, high) = mean_columns
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| {
            (low.min(*value), high.max(*value))
        });
    assert!(
        high - low >= 2.5,
        "the edge does not zigzag: {mean_columns:?}"
    );
    // The arrowhead is intact: one pixel inside the tip, the middle scanline is the read and
    // the top scanline is cut away; the body before the edge is the read's colour.
    let tip_column = (boundary - 1.0) as u32;
    assert_eq!(rgb_at(&image, tip_column, 15), body);
    assert_eq!(rgb_at(&image, tip_column, 10), white);
    assert_eq!(rgb_at(&image, (edge_left - 20.0) as u32, 15), body);
    eprintln!(
        "soft clip sawtooth on {}: edge {edge_left:.2}..{edge_right:.2}, scanline means {mean_columns:?}",
        gpu.description
    );
}

/// A reverse read's leading hard clip on the real GPU: a solid two-point bar in the cut-edge
/// colour on every scanline, just inside the arrowhead that points left from the boundary.
#[test]
fn hard_clip_bar_is_solid_and_sits_inside_the_arrowhead_on_the_gpu() {
    use crate::palette::PaletteToken;
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    let scene = boundary_marker_scene(&ledger);
    let origin = BOUNDARY_FIXTURE_ORIGIN;
    // The reverse `H3 M10` read starts (arrowhead and clip) at origin + 1200, drawn at 80.4.
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), origin + 1199, origin + 1203).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let image = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 1);
    let (oracle, expected) = oracle_for(&scene, camera);
    assert!(
        expected.iter().any(
            |instance| instance.shape == AlignmentGpuShape::HardClipEdgeStartBehindArrow as u32
        )
    );
    oracle.assert_matches(&image, "hard clip bar behind the arrowhead");

    let boundary = CLIP.x + 64.0;
    let (bar_left, bar_right) = (boundary + 6.05, boundary + 6.05 + 2.0);
    let edge = light_rgb(PaletteToken::CLIP_EDGE);
    let painted_columns = |row: u32| {
        (0..TARGET[0])
            .filter(|column| close(rgb_at(&image, *column, row), edge))
            .collect::<Vec<_>>()
    };
    let expected_columns = (super::first_column_at_or_after(bar_left) as u32
        ..super::first_column_at_or_after(bar_right) as u32)
        .collect::<Vec<_>>();
    assert_eq!(expected_columns.len(), 2);
    for row in 10..=20_u32 {
        assert_eq!(painted_columns(row), expected_columns, "scanline {row}");
    }
    // Left of the bar is the arrowhead: the read colour on the middle scanline, cut away at
    // the top, one pixel inside the tip.
    let tip_column = (boundary + 1.0) as u32;
    assert_ne!(rgb_at(&image, tip_column, 15), [255, 255, 255]);
    assert_eq!(rgb_at(&image, tip_column, 10), [255, 255, 255]);
    eprintln!(
        "hard clip bar on {}: columns {expected_columns:?}",
        gpu.description
    );
}

/// Soft-clipped ghost bases on the real GPU at base detail: translucent cells beyond the cut
/// edge where the row is empty, and covered by a neighbouring read that starts on one of them.
#[test]
fn ghost_bases_are_not_drawn_until_their_letters_fit_on_the_gpu() {
    use crate::alignment_scene::tests::test_scene_detailed;
    use rgv_alignment_scene::GeometryLod;
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    // `M7 S3` over [10, 17): three ghost cells at base LOD, as in the zoomed-in test above.
    let reads = [TestRead {
        start: 10,
        reverse: false,
        cigar: cigar(&[(CigarOpKind::Match, 7), (CigarOpKind::SoftClip, 3)]),
    }];
    let scene = test_scene_detailed(
        &ledger,
        GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
        100,
        NonZeroU64::new(8).unwrap(),
        &reads,
        NonZeroU32::new(4).unwrap(),
        None,
        GeometryLod::Base,
    );
    // 256 points over 50 bases: 5.12 points per base, base LOD but too narrow for letters.
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 50).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    assert!(
        !rgv_alignment_scene::clip_ghost_visible(5.12),
        "5.12 points per base is below the ghost threshold"
    );
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let image = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 1);
    let (oracle, expected) = oracle_for(&scene, camera);
    // The ghost instances are still packed (packing does not depend on the zoom) but each
    // projects to an empty quad, so the GPU and the oracle draw nothing for them.
    let ghosts = expected
        .iter()
        .filter(|instance| instance.shape == AlignmentGpuShape::ClipGhostCell as u32)
        .collect::<Vec<_>>();
    assert_eq!(
        ghosts.len(),
        3,
        "the page still carries the three ghost cells"
    );
    assert!(
        ghosts.iter().all(|ghost| ghost.quad.width <= 0.0),
        "no ghost quad has area below the letter threshold"
    );
    oracle.assert_matches(&image, "ghost bases hidden while their letters do not fit");
    let frame = rgv_alignment_scene::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
    assert!(
        frame
            .primitives
            .iter()
            .all(|primitive| primitive.shape
                != rgv_alignment_scene::CpuPrimitiveShape::ClipGhostCell),
        "the CPU projector (picking and export) hides ghost cells too"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn ghost_bases_show_in_empty_space_and_under_a_neighbouring_read_on_the_gpu() {
    use crate::alignment_scene::tests::test_scene_detailed;
    use rgv_alignment_scene::{AlignmentComponent, GeometryLod};
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    // A: `M5 S5` over [12, 17), SEQ ACGTA|CGTAC, ghosts at 17..22. B: [20, 30) in the same
    // row (three bases after A, the minimum spacing of reads in a row), on top of A's fourth
    // and fifth ghosts.
    let reads = [
        TestRead {
            start: 12,
            reverse: false,
            cigar: cigar(&[(CigarOpKind::Match, 5), (CigarOpKind::SoftClip, 5)]),
        },
        TestRead::forward(20),
    ];
    let scene = test_scene_detailed(
        &ledger,
        GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
        100,
        NonZeroU64::new(8).unwrap(),
        &reads,
        NonZeroU32::new(4).unwrap(),
        None,
        GeometryLod::Base,
    );
    // 32 points per base over 14..22: base p starts at 16.4 + 32 (p - 14).
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 14, 22).unwrap(),
        0.0,
        14.0,
        11.0,
        CLIP,
    )
    .unwrap();
    let mut ghost_rows = Vec::new();
    for page in scene.visible_pages(camera.interval(), camera.visible_rows()) {
        for primitive in page.primitives() {
            if let AlignmentComponent::ClipGhostBase { obscured, .. } = primitive.component {
                ghost_rows.push((
                    page.interval().start() + primitive.start_offset,
                    page.row_range().start + primitive.row_offset,
                    obscured,
                ));
            }
        }
    }
    ghost_rows.sort_unstable();
    assert_eq!(
        ghost_rows,
        [
            (17, 0, false),
            (18, 0, false),
            (19, 0, false),
            (20, 0, true),
            (21, 0, true)
        ]
    );

    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    let image = render_scene(&gpu, &mut renderer, &scene, camera, &ledger, 1);
    let (oracle, expected) = oracle_for(&scene, camera);
    assert_eq!(
        expected
            .iter()
            .filter(|instance| instance.shape == AlignmentGpuShape::ClipGhostCell as u32)
            .count(),
        5
    );
    oracle.assert_matches(&image, "soft clip ghost bases");

    let frame = rgv_alignment_scene::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
    let handle_at = |position: u64| {
        frame
            .primitives
            .iter()
            .find(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                ) && primitive.x <= CLIP.x + 32.0 * (position as f64 - 14.0) + 1.0
                    && primitive.x + primitive.width
                        >= CLIP.x + 32.0 * (position as f64 - 13.0) - 1.0
            })
            .unwrap()
            .handle
    };
    let a = body_rgb(&scene, camera, handle_at(15));
    let b = body_rgb(&scene, camera, handle_at(21));
    // The ghost colour over the white target: the read colour moved half way to the ghost
    // slot, at the ghost slot's alpha.
    let tint = AlignmentPalette::light().color(crate::palette::PaletteToken::CLIP_GHOST);
    let ghost = std::array::from_fn::<u8, 3, _>(|channel| {
        let read = f64::from(a[channel]) / 255.0;
        let mixed = read + (f64::from(tint[channel]) - read) * 0.5;
        let alpha = f64::from(tint[3]);
        ((mixed * alpha + (1.0 - alpha)) * 255.0).round() as u8
    });
    let cell_centre = |position: u64| (CLIP.x + 32.0 * (position as f64 - 14.0) + 16.0) as u32;
    for position in [17, 18, 19] {
        let pixel = rgb_at(&image, cell_centre(position), 15);
        assert!(
            close(pixel, ghost),
            "ghost {position}: {pixel:?} vs {ghost:?}"
        );
        assert_ne!(pixel, a);
        assert_ne!(pixel, [255, 255, 255]);
    }
    // Positions 20 and 21 hold A's fourth and fifth ghosts, but B is drawn over them: its
    // first base matches the reference, so 20 is B's own colour, and its second (`C` against
    // the all-`A` reference) is B's mismatch cell, not the ghost.
    assert_eq!(rgb_at(&image, cell_centre(20), 15), b);
    let covered = rgb_at(&image, cell_centre(21), 15);
    assert!(
        !close(covered, ghost),
        "ghost 21 shows through B: {covered:?}"
    );
    assert_ne!(covered, [255, 255, 255]);
    eprintln!(
        "ghost bases on {}: ghost {ghost:?}, covered cell {b:?}",
        gpu.description
    );
}

/// The reported defect on the real GPU: zoomed out from base detail (about 2 to 10 points per
/// base), a read's arrow end showed the block's arrowhead plus a smaller point protruding past
/// it, because the aligned block, the one-base direction terminal and the base cell at the tip
/// each sized their head from their own width. They now share one diagonal, so scanning the
/// columns from the tip inwards the painted height is one contiguous run that grows
/// monotonically to the full read height, with no step or notch, column for column as the CPU
/// projector (which picking and export use) says.
#[test]
#[allow(clippy::too_many_lines)]
fn read_tips_are_one_clean_arrowhead_on_the_gpu_when_zoomed_out() {
    use crate::alignment_scene::tests::test_scene_detailed;
    use rgv_alignment_scene::{AlignmentComponent, GeometryLod};
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    // Forward [40, 50) and reverse [60, 70). Against the all-`A` reference of the base-detail
    // scene the fixture sequence `ACGTACGTAC` puts a mismatch cell on the forward tip's base
    // (and on others further in).
    let reads = [
        TestRead::forward(40),
        TestRead {
            start: 60,
            reverse: true,
            cigar: cigar(&[(CigarOpKind::Match, 10)]),
        },
    ];
    let interval = GenomicInterval::new(ContigId::new(0), 0, 200).unwrap();
    let scene_at = |lod| {
        test_scene_detailed(
            &ledger,
            interval,
            200,
            // Two tiles; no page boundary falls inside a head.
            NonZeroU64::new(100).unwrap(),
            &reads,
            NonZeroU32::new(2).unwrap(),
            None,
            lod,
        )
    };
    let read_scene = scene_at(GeometryLod::Read);
    let base_scene = scene_at(GeometryLod::Base);
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    // (scene, camera interval, row and read height). At read detail tall rows give a 13.2-point
    // head over thirteen pixel columns. At base detail the rows are the usual 11 points (a
    // 6.05-point head) and the zoom keeps the next mismatch cells behind the head.
    let cases = [
        (
            false,
            0,
            128,
            30.0,
            24.0,
            "read detail at 2 points per base",
        ),
        (
            false,
            20,
            84,
            30.0,
            24.0,
            "read detail at 4 points per base",
        ),
        (true, 34, 66, 14.0, 11.0, "base detail at 8 points per base"),
        (
            true,
            40,
            65,
            14.0,
            11.0,
            "base detail at 10.24 points per base",
        ),
    ];
    for (index, (base_detail, start, end, row_height, read_height, label)) in
        cases.into_iter().enumerate()
    {
        let scene = if base_detail {
            &base_scene
        } else {
            &read_scene
        };
        let camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), start, end).unwrap(),
            0.0,
            row_height,
            read_height,
            CLIP,
        )
        .unwrap();
        let image = render_scene(
            &gpu,
            &mut renderer,
            scene,
            camera,
            &ledger,
            index as u64 + 1,
        );
        let (oracle, _) = oracle_for(scene, camera);
        oracle.assert_matches(&image, label);

        let frame = rgv_alignment_scene::CpuAlignmentProjector::project(scene, 1, camera).unwrap();
        let mut arrows = Vec::new();
        for primitive in frame.primitives.iter() {
            if let Some(arrow) = primitive.arrow {
                if let Some((_, seen)) = arrows
                    .iter()
                    .find(|(handle, _)| *handle == primitive.handle)
                {
                    // Every tip primitive of a read carries the same arrow.
                    assert_eq!(*seen, arrow, "{label}");
                } else {
                    arrows.push((primitive.handle, arrow));
                }
            }
        }
        assert_eq!(arrows.len(), 2, "{label}: {arrows:?}");
        if base_detail {
            assert!(
                frame
                    .primitives
                    .iter()
                    .any(|primitive| primitive.arrow.is_some()
                        && matches!(primitive.component, AlignmentComponent::Base { .. }))
            );
        }
        for (handle, arrow) in arrows {
            let head = arrow.length();
            assert!((head - read_height * 0.55).abs() < 1.0e-9);
            let top = arrow.centre_y - read_height * 0.5;
            let band =
                (top.floor() as u32).saturating_sub(2)..((top + read_height).ceil() as u32 + 2);
            let mut heights = Vec::new();
            // Pixel columns whose centres lie from just outside the tip to past the head.
            for step in -2..(head as i32 + 4) {
                let column = if arrow.points_right {
                    (arrow.tip_x - 0.5).floor() as i64 - i64::from(step)
                } else {
                    (arrow.tip_x - 0.5).ceil() as i64 + i64::from(step)
                };
                let column = u32::try_from(column).unwrap();
                let centre_x = f64::from(column) + 0.5;
                let distance = arrow.distance_from_tip(centre_x);
                let painted = band
                    .clone()
                    .filter(|row| image.get_pixel(column, *row).0 != [255, 255, 255, 255])
                    .collect::<Vec<_>>();
                // One contiguous run: no notch.
                if let (Some(first), Some(last)) = (painted.first(), painted.last()) {
                    assert_eq!(
                        (last - first + 1) as usize,
                        painted.len(),
                        "{label}: column {column} is notched: {painted:?}"
                    );
                }
                // Column for column what the CPU projector covers, wherever no pixel centre in
                // the column is within a hair of the diagonal.
                let mut ambiguous = false;
                let cpu = band
                    .clone()
                    .filter(|row| {
                        let centre_y = f64::from(*row) + 0.5;
                        if let Some(outline) = arrow.outline_distance_at(centre_y) {
                            ambiguous |= (outline - distance).abs() < 1.0e-3;
                        }
                        frame
                            .primitives
                            .iter()
                            .filter(|primitive| primitive.handle == handle)
                            .any(|primitive| primitive.contains(centre_x, centre_y))
                    })
                    .count();
                if !ambiguous {
                    assert_eq!(
                        painted.len(),
                        cpu,
                        "{label}: column {column} ({distance:.2} from the tip)"
                    );
                }
                heights.push((distance, painted.len()));
            }
            // From the tip inwards the painted height never drops: one diagonal, no stub.
            for pair in heights.windows(2) {
                assert!(
                    pair[0].1 <= pair[1].1,
                    "{label}: the arrow steps between {pair:?}: {heights:?}"
                );
            }
            assert_eq!(
                heights[0].1, 0,
                "{label}: painted beyond the tip: {heights:?}"
            );
            assert_eq!(
                heights.last().unwrap().1,
                read_height as usize,
                "{label}: {heights:?}"
            );
            // Within the head the height grows in many small steps, not one jump.
            let within_head = heights
                .iter()
                .filter(|(distance, height)| *distance < head && *height > 0)
                .count();
            assert!(within_head + 1 >= head as usize, "{label}: {heights:?}");
            eprintln!(
                "{label} on {}: tip at {:.2}, column heights {:?}",
                gpu.description,
                arrow.tip_x,
                heights
                    .iter()
                    .map(|(_, height)| *height)
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// The reported screenshot on the real GPU: a reverse read at about 3.7 points per base with
/// mismatches in its first few bases. The cells short of the tip were drawn full height inside
/// the arrowhead, poking out of its cut-away corners; each now carries its distance from the tip
/// and is cut by the read's one diagonal. No mismatch-coloured pixel (indeed no painted pixel)
/// lies outside the arrow outline, cut cells still show inside it, and the frame matches the
/// CPU oracle pixel for pixel.
#[test]
#[allow(clippy::too_many_lines)]
fn mismatch_cells_near_a_read_tip_stay_inside_its_arrowhead_on_the_gpu() {
    use crate::alignment_scene::tests::test_scene_detailed;
    use crate::palette::PaletteToken;
    use rgv_alignment_scene::{AlignmentComponent, AlignmentGeometryToken, GeometryLod};
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    // Reverse [60, 70), tip 60. Against the all-`A` reference the fixture sequence `ACGTACGTAC`
    // puts mismatch cells 1, 2, 3, 5, 6, 7 and 9 bases from the tip.
    let reads = [TestRead {
        start: 60,
        reverse: true,
        cigar: cigar(&[(CigarOpKind::Match, 10)]),
    }];
    let scene = test_scene_detailed(
        &ledger,
        GenomicInterval::new(ContigId::new(0), 0, 200).unwrap(),
        200,
        NonZeroU64::new(100).unwrap(),
        &reads,
        NonZeroU32::new(2).unwrap(),
        None,
        GeometryLod::Base,
    );
    let mismatch_colours = [b'C', b'G', b'T'].map(|base| light_rgb(PaletteToken::base(base)));
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    // 256 points over 69 bases: 3.71 points per base. The usual 11-point read has a 6.05-point
    // head (1.6 bases); a 24-point one a 13.2-point head (3.6 bases).
    for (index, (row_height, read_height)) in [(14.0, 11.0), (30.0, 24.0)].into_iter().enumerate() {
        let label = format!("{read_height}-point read at 3.71 points per base");
        let camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 30, 99).unwrap(),
            0.0,
            row_height,
            read_height,
            CLIP,
        )
        .unwrap();
        let image = render_scene(
            &gpu,
            &mut renderer,
            &scene,
            camera,
            &ledger,
            index as u64 + 1,
        );
        let (oracle, expected) = oracle_for(&scene, camera);
        oracle.assert_matches(&image, &label);
        // The cells 1, 2 and 3 bases in are instances cut by the arrow, with their distance.
        assert!(
            expected
                .iter()
                .filter(|instance| instance.shape == AlignmentGpuShape::DirectionTerminalLeft as u32)
                .count()
                >= 3,
            "{label}"
        );

        let frame = rgv_alignment_scene::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
        let arrow = frame
            .primitives
            .iter()
            .find_map(|primitive| primitive.arrow)
            .unwrap();
        let near_cells = frame
            .primitives
            .iter()
            .filter(|primitive| {
                matches!(primitive.component, AlignmentComponent::Base { .. })
                    && matches!(
                        primitive.geometry,
                        AlignmentGeometryToken::InsideArrowhead(_)
                    )
            })
            .count();
        assert_eq!(near_cells, 7, "{label}");
        assert!(!arrow.points_right);
        let body = body_rgb(&scene, camera, frame.primitives[0].handle);
        assert!(
            mismatch_colours
                .iter()
                .all(|colour| !close(*colour, body) && !close(*colour, [255, 255, 255]))
        );
        // Written out independently: min(0.55 x height, 10 bases) from the tip, covering
        // |y - centre| <= height / 2 x min(d / head, 1).
        let head = (read_height * 0.55).min(10.0 * 256.0 / 69.0);
        assert!((arrow.length() - head).abs() < 1.0e-9);
        let half = read_height * 0.5;
        let top = arrow.centre_y - half;
        let (mut outside, mut cut_cell_pixels) = (0, 0);
        for column in (arrow.tip_x - 3.0).floor() as u32..(arrow.tip_x + head + 4.0) as u32 {
            for row in (top - 2.0).floor() as u32..(top + read_height + 2.0).ceil() as u32 {
                let (x, y) = (f64::from(column) + 0.5, f64::from(row) + 0.5);
                let distance = x - arrow.tip_x;
                let offset = (y - arrow.centre_y).abs();
                let outline = half * (distance / head).min(1.0);
                // Pixel centres within a hair of the outline are not decided by either side.
                if (offset - outline).abs() < 1.0e-3 || (offset - half).abs() < 1.0e-3 {
                    continue;
                }
                let inside = distance > 0.0 && offset < half && offset <= outline;
                let pixel = rgb_at(&image, column, row);
                let mismatch = mismatch_colours.iter().any(|colour| close(*colour, pixel));
                if !inside {
                    assert!(
                        !mismatch,
                        "{label}: mismatch colour {pixel:?} outside the arrow at ({x}, {y}), \
                         {distance:.2} from the tip"
                    );
                    if pixel != [255, 255, 255] {
                        outside += 1;
                    }
                } else if mismatch && distance < head {
                    cut_cell_pixels += 1;
                }
            }
        }
        assert_eq!(
            outside, 0,
            "{label}: painted pixels outside the arrow outline"
        );
        // The cut cells are still drawn within the head.
        assert!(cut_cell_pixels > 0, "{label}");
        eprintln!(
            "{label} on {}: head {head:.2} points, {cut_cell_pixels} mismatch pixels within it, \
             none outside",
            gpu.description
        );
    }
}

/// Zoomed out, the marks on a read scale with it on the real GPU: at 0.4 points per base a
/// 101-base read keeps a 4-point arrowhead (ten bases, not 0.55 x its 24-point height) and its
/// insertion is a one-point hairline; at 0.08 the head is 0.8 points and the insertion is not
/// drawn; at 0.04 the head would be under half a point, so the read ends square. Every frame
/// matches the CPU oracle pixel for pixel, and the arrow's columns match the CPU projector.
#[test]
#[allow(clippy::too_many_lines)]
fn zoomed_out_reads_keep_a_short_arrowhead_and_a_hairline_insertion_on_the_gpu() {
    use rgv_alignment_scene::AlignmentComponent;
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(256 << 20, 32 << 20, 32 << 20);
    // A forward 50M 2I 51M read at 100 (its insertion between 149 and 150), a reverse 101M
    // read at 300 and a forward 101M 5S read at 500, whose soft clip shares the arrow's end.
    let reads = [
        TestRead {
            start: 100,
            reverse: false,
            cigar: cigar(&[
                (CigarOpKind::Match, 50),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 51),
            ]),
        },
        TestRead {
            start: 300,
            reverse: true,
            cigar: cigar(&[(CigarOpKind::Match, 101)]),
        },
        TestRead {
            start: 500,
            reverse: false,
            cigar: cigar(&[(CigarOpKind::Match, 101), (CigarOpKind::SoftClip, 5)]),
        },
    ];
    let interval = GenomicInterval::new(ContigId::new(0), 0, 8_000).unwrap();
    let scene = test_scene_over(
        &ledger,
        interval,
        8_000,
        NonZeroU64::new(2_000).unwrap(),
        &reads,
        NonZeroU32::new(2).unwrap(),
    );
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);
    // (camera end over the 256-point pane, points per base, head length, insertion width)
    let cases = [
        (640, 0.4, 4.0, Some(1.0)),
        (3_200, 0.08, 0.8, None),
        (6_400, 0.04, 0.0, None),
    ];
    for (index, (end, points_per_base, head, insertion_width)) in cases.into_iter().enumerate() {
        let label = format!("{points_per_base} points per base");
        let camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, end).unwrap(),
            0.0,
            30.0,
            24.0,
            CLIP,
        )
        .unwrap();
        assert!((1.0 / camera.bases_per_pixel() - points_per_base).abs() < 1.0e-12);
        let image = render_scene(
            &gpu,
            &mut renderer,
            &scene,
            camera,
            &ledger,
            index as u64 + 1,
        );
        let (oracle, expected) = oracle_for(&scene, camera);
        let comparison = oracle.assert_matches(&image, &label);

        // The insertion: a one-point hairline on its boundary, or no quad at all.
        let markers = expected
            .iter()
            .filter(|instance| instance.shape == AlignmentGpuShape::PointMarker as u32)
            .collect::<Vec<_>>();
        assert_eq!(markers.len(), 1, "{label}");
        let boundary = CLIP.x + 150.0 * points_per_base;
        let frame = rgv_alignment_scene::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
        let cpu_insertion = frame
            .primitives
            .iter()
            .find(|primitive| matches!(primitive.component, AlignmentComponent::Insertion { .. }));
        if let Some(width) = insertion_width {
            let quad = markers[0].quad;
            assert!((quad.width - width).abs() < 1.0e-9, "{label}: {quad:?}");
            assert!((quad.x + width * 0.5 - boundary).abs() < 1.0e-9);
            let cpu = cpu_insertion.expect("the hairline is projected for picking");
            assert!((cpu.x - quad.x).abs() < 1.0e-9 && (cpu.width - width).abs() < 1.0e-9);
            // On the GPU the hairline's pixel column differs from the read body beside it.
            let column = boundary.floor() as u32;
            let middle = (quad.y + quad.height * 0.5) as u32;
            assert_ne!(
                image.get_pixel(column, middle).0,
                image.get_pixel(column - 4, middle).0,
                "{label}: no hairline at column {column}"
            );
        } else {
            assert!(markers[0].quad.width.abs() < f64::EPSILON, "{label}");
            assert!(cpu_insertion.is_none(), "{label}: {cpu_insertion:?}");
        }
        // The soft clip behind the arrowhead: a one-point plain hairline just inside the head's
        // base (the oracle above checks it is solid, not a sawtooth), or nothing.
        let clip = expected
            .iter()
            .find(|instance| instance.shape == AlignmentGpuShape::SoftClipEdgeEndBehindArrow as u32)
            .expect("the clip edge instance");
        let cpu_clip = frame
            .primitives
            .iter()
            .find(|primitive| matches!(primitive.component, AlignmentComponent::SoftClip { .. }));
        if let Some(width) = insertion_width {
            let tip = CLIP.x + 601.0 * points_per_base;
            assert!((clip.quad.width - width).abs() < 1.0e-9, "{label}");
            assert!(
                (clip.quad.x + width - (tip - head)).abs() < 1.0e-9,
                "{label}"
            );
            let cpu = cpu_clip.expect("the hairline is projected for picking");
            assert_eq!(
                cpu.shape,
                rgv_alignment_scene::CpuPrimitiveShape::BoundaryHairline
            );
            assert!((cpu.x - clip.quad.x).abs() < 1.0e-9, "{label}");
        } else {
            assert!(clip.quad.width.abs() < f64::EPSILON, "{label}");
            assert!(cpu_clip.is_none(), "{label}: {cpu_clip:?}");
        }

        // One arrow per read, `head` points long, column for column what the CPU projector
        // covers; within the head the painted height grows from the tip, and a zero head is a
        // square end.
        let mut arrows = Vec::new();
        for primitive in frame.primitives.iter() {
            if let Some(arrow) = primitive.arrow
                && !arrows.iter().any(|(handle, _)| *handle == primitive.handle)
            {
                arrows.push((primitive.handle, arrow));
            }
        }
        assert_eq!(arrows.len(), 3, "{label}");
        for (handle, arrow) in arrows {
            assert!((arrow.length() - head).abs() < 1.0e-9, "{label}: {arrow:?}");
            let top = arrow.centre_y - 12.0;
            let band = (top.floor() as u32).saturating_sub(2)..((top + 24.0).ceil() as u32 + 2);
            let mut heights = Vec::new();
            for step in 0..4_i64 {
                let column = if arrow.points_right {
                    (arrow.tip_x - 0.5).floor() as i64 - step
                } else {
                    (arrow.tip_x - 0.5).ceil() as i64 + step
                };
                let column = u32::try_from(column).unwrap();
                let centre_x = f64::from(column) + 0.5;
                let distance = arrow.distance_from_tip(centre_x);
                let painted = band
                    .clone()
                    .filter(|row| image.get_pixel(column, *row).0 != [255, 255, 255, 255])
                    .count();
                let mut ambiguous = false;
                let cpu = band
                    .clone()
                    .filter(|row| {
                        let centre_y = f64::from(*row) + 0.5;
                        if let Some(outline) = arrow.outline_distance_at(centre_y) {
                            ambiguous |= (outline - distance).abs() < 1.0e-3;
                        }
                        frame
                            .primitives
                            .iter()
                            .filter(|primitive| primitive.handle == handle)
                            .any(|primitive| primitive.contains(centre_x, centre_y))
                    })
                    .count();
                if !ambiguous {
                    assert_eq!(painted, cpu, "{label}: column {column} ({distance:.2})");
                }
                heights.push((distance, painted));
            }
            for pair in heights.windows(2) {
                assert!(pair[0].1 <= pair[1].1, "{label}: {heights:?}");
            }
            if head == 0.0 {
                // Square: the first column inside the tip is already full height.
                assert_eq!(heights[0].1, 24, "{label}: {heights:?}");
            } else {
                // A short head: cut within `head` of the tip (a head under a pixel may have no
                // column centre inside it), full height from `head` inwards.
                for (distance, height) in &heights {
                    if *distance > head {
                        assert_eq!(*height, 24, "{label}: {heights:?}");
                    } else {
                        assert!(*height < 24, "{label}: {heights:?}");
                    }
                }
            }
            eprintln!(
                "{label} on {}: head {head}, tip at {:.2}, column heights {:?}; {} pixels \
                 compared",
                gpu.description,
                arrow.tip_x,
                heights
                    .iter()
                    .map(|(_, height)| *height)
                    .collect::<Vec<_>>(),
                comparison.compared
            );
        }
    }
}

/// The harness's independent I-beam fragment rule agrees with the CPU projector's
/// `insertion_mark_covers` over the whole box, for every bar width and several read heights.
#[test]
fn insertion_fragment_mirror_agrees_with_the_cpu_cover_rule() {
    for width in [7.0, 7.5, 8.0] {
        for height in [1.0, 5.0, 11.0, 20.0] {
            for step_x in 0..80 {
                let x = width * (f64::from(step_x) + 0.5) / 80.0;
                for step_y in 0..80 {
                    let y = height * (f64::from(step_y) + 0.5) / 80.0;
                    for shape in [
                        AlignmentGpuShape::PointMarker,
                        AlignmentGpuShape::PointMarkerAtEnd,
                    ] {
                        assert_eq!(
                            fragment_is_kept(shape as u32, [x, y], [width, height], 50.0, 0),
                            rgv_alignment_scene::insertion_mark_covers(x, y, width, height),
                            "{width} x {height} at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}
