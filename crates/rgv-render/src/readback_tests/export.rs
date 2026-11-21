use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentCamera, AlignmentInteractionState, AlignmentPresentationScene, AllocationLedger,
    CpuClipRect,
};
use rgv_core::{ContigId, GenomicInterval, Viewport};
use rgv_layout::{DisplayList, Rgba, TextPrimitive};

use super::{
    FORMAT, Oracle, TestGpu, rectangle::paint_rectangle, rectangle::shape_gallery, test_gpu,
};
use crate::{
    AlignmentGpuRenderer, AlignmentGpuSceneRequest, AlignmentGpuScissor, GpuExportError,
    GpuExportLayout, GpuExportTarget, GpuExportTile, PngExportBackend,
    alignment_scene::tests::{TestRead, test_scene_over},
    export_png_scaled, export_png_scaled_prefer_gpu,
    gpu_export::render_display_list_with_layout,
    render_display_list_gpu, render_gpu_export,
};

const FRAME_POINTS: [u32; 2] = [160, 96];

fn gallery() -> DisplayList {
    DisplayList {
        rectangles: shape_gallery(),
        texts: Vec::new(),
    }
}

#[test]
fn gpu_display_list_export_matches_the_readback_oracle_at_every_scale() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let display = gallery();
    for scale in [1.0_f32, 2.0, 3.0] {
        let image = render_display_list_gpu(
            &gpu.device,
            &gpu.queue,
            &display,
            FRAME_POINTS[0],
            FRAME_POINTS[1],
            scale,
        )
        .unwrap();
        let size = [
            (FRAME_POINTS[0] as f32 * scale) as u32,
            (FRAME_POINTS[1] as f32 * scale) as u32,
        ];
        assert_eq!(image.dimensions(), (size[0], size[1]));
        // The export is the screen's neutral rectangle pass: same oracle as the pipeline test.
        let mut oracle = Oracle::new(size);
        for rectangle in &display.rectangles {
            paint_rectangle(
                &mut oracle,
                *rectangle,
                f64::from(scale),
                [0, 0, size[0], size[1]],
                None,
                None,
            );
        }
        let comparison = oracle.assert_matches(&image, &format!("GPU export at {scale}x"));
        eprintln!(
            "GPU display-list export at {scale}x on {}: {} pixels compared, {} painted",
            gpu.description, comparison.compared, comparison.painted
        );
    }
}

#[test]
fn tiled_gpu_display_list_export_has_no_seams_against_the_untiled_export() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let display = gallery();
    let untiled = render_display_list_gpu(
        &gpu.device,
        &gpu.queue,
        &display,
        FRAME_POINTS[0],
        FRAME_POINTS[1],
        2.0,
    )
    .unwrap();
    // A 64-pixel tile edge stands in for a device whose textures are smaller than the export:
    // 320x192 pixels become a 5x3 grid, and every shape straddles several tiles.
    let layout = GpuExportLayout::new(FRAME_POINTS[0], FRAME_POINTS[1], 2.0, 64).unwrap();
    assert_eq!(layout.tile_count(), 15);
    let tiled = render_display_list_with_layout(&gpu.device, &gpu.queue, &display, layout).unwrap();
    assert_eq!(tiled.dimensions(), untiled.dimensions());
    // Shifting a rectangle into tile coordinates rounds its f32 origin, so a pixel whose centre
    // sits exactly on a shape edge may be decided differently; nothing else may change. The
    // oracle knows which pixels those are.
    let size = [tiled.width(), tiled.height()];
    let mut oracle = Oracle::new(size);
    for rectangle in &display.rectangles {
        paint_rectangle(
            &mut oracle,
            *rectangle,
            2.0,
            [0, 0, size[0], size[1]],
            None,
            None,
        );
    }
    oracle.assert_matches(&tiled, "tiled GPU export");
    let seams = tiled
        .enumerate_pixels()
        .zip(untiled.pixels())
        .filter(|((x, y, tiled), untiled)| {
            // Anti-aliased arc coverage may round to a neighbouring 8-bit value.
            (0..4).any(|channel| {
                tiled.0[channel].abs_diff(untiled.0[channel]) > super::CHANNEL_TOLERANCE
            }) && !oracle.is_uncertain(*x, *y)
        })
        .count();
    assert_eq!(seams, 0, "tile seams must not be visible");
}

#[test]
fn gpu_export_writes_a_png_and_agrees_with_the_cpu_rasteriser_on_hard_edged_geometry() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    // Hard-edged geometry and text only: the CPU rasteriser has no anti-aliased arcs.
    let mut display = DisplayList::synthetic_coverage_and_reads_at(320.0, 24, 1_000, 8.0);
    display.texts.push(TextPrimitive {
        x: 160.0,
        y: 12.0,
        text: "chr1:1,000-2,000".to_owned(),
        font_size: 11.0,
        color: Rgba::new(0.1, 0.1, 0.1, 1.0),
    });
    let gpu_path = directory.path().join("gpu.png");
    let cpu_path = directory.path().join("cpu.png");
    let backend = export_png_scaled_prefer_gpu(
        Some((&gpu.device, &gpu.queue)),
        &display,
        320,
        240,
        2.0,
        &gpu_path,
    )
    .unwrap();
    assert_eq!(backend, PngExportBackend::Gpu);
    export_png_scaled(&display, 320, 240, 2.0, &cpu_path).unwrap();
    let gpu_image = image::open(gpu_path).unwrap().to_rgba8();
    let cpu_image = image::open(cpu_path).unwrap().to_rgba8();
    assert_eq!(gpu_image.dimensions(), (640, 480));
    assert_eq!(cpu_image.dimensions(), (640, 480));
    let differing = gpu_image
        .pixels()
        .zip(cpu_image.pixels())
        .filter(|(gpu, cpu)| {
            (0..4).any(|channel| gpu.0[channel].abs_diff(cpu.0[channel]) > super::CHANNEL_TOLERANCE)
        })
        .count();
    // Only pixels whose centre sits on a shape boundary may be decided differently.
    assert!(
        differing * 200 < (640 * 480),
        "{differing} pixels differ between the GPU and CPU exports"
    );
    eprintln!(
        "GPU and CPU exports of the synthetic frame on {}: {differing} of {} pixels differ",
        gpu.description,
        640 * 480
    );
}

#[test]
fn export_target_rejects_unsupported_formats_and_foreign_tiles() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let layout = GpuExportLayout::new(64, 32, 1.0, 8_192).unwrap();
    assert!(matches!(
        GpuExportTarget::new(&gpu.device, layout, wgpu::TextureFormat::Rgba16Float),
        Err(GpuExportError::UnsupportedFormat(_))
    ));
    let mut target = GpuExportTarget::new(&gpu.device, layout, FORMAT).unwrap();
    let foreign = GpuExportTile {
        index: 0,
        origin_pixels: [32, 0],
        size_pixels: [64, 32],
        scale: 1.0,
    };
    assert!(matches!(
        target.render_tile(
            &gpu.device,
            &gpu.queue,
            &foreign,
            wgpu::Color::WHITE,
            |_| { Ok::<(), std::convert::Infallible>(()) }
        ),
        Err(GpuExportError::ForeignTile)
    ));
    // A draw error is reported and leaves the target usable.
    let tile = layout.tiles()[0];
    assert!(matches!(
        target.render_tile(&gpu.device, &gpu.queue, &tile, wgpu::Color::WHITE, |_| {
            Err("lane frame is stale")
        }),
        Err(GpuExportError::Draw(_))
    ));
    assert_eq!(target.rendered_tiles(), 0);
    target
        .render_tile(&gpu.device, &gpu.queue, &tile, wgpu::Color::BLACK, |_| {
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
    assert_eq!(target.rendered_tiles(), 1);
    assert!(
        target
            .into_image()
            .pixels()
            .all(|pixel| pixel.0 == [0, 0, 0, 255])
    );
    // A texture larger than the device allows is an error, not an uncaptured device panic.
    let limit = gpu.device.limits().max_texture_dimension_2d;
    let oversized = GpuExportLayout::new(limit + 1, 8, 1.0, u32::MAX);
    if let Ok(oversized) = oversized {
        assert!(matches!(
            GpuExportTarget::new(&gpu.device, oversized, FORMAT),
            Err(GpuExportError::Device(_))
        ));
    }
}

const LANE_POINTS: [f32; 4] = [10.0, 6.0, 128.0, 48.0];

fn scene(ledger: &AllocationLedger) -> Arc<AlignmentPresentationScene> {
    let reads = [1_u64, 3, 2, 9, 14]
        .into_iter()
        .enumerate()
        .map(|(index, start)| TestRead {
            reverse: index % 2 == 1,
            ..TestRead::forward(start + 4)
        })
        .collect::<Vec<_>>();
    test_scene_over(
        ledger,
        GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
        100,
        NonZeroU64::new(8).unwrap(),
        &reads,
        NonZeroU32::new(2).unwrap(),
    )
}

/// Renders the alignment-scene lane into an export of `layout`, capturing one frame per tile
/// with the tile transform, the way the desktop exports a page pipeline.
fn export_scene(
    gpu: &TestGpu,
    renderer: &mut AlignmentGpuRenderer,
    scene: &Arc<AlignmentPresentationScene>,
    ledger: &AllocationLedger,
    layout: GpuExportLayout,
) -> image::RgbaImage {
    let interaction = AlignmentInteractionState::default();
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 2, 26).unwrap(),
        f64::from(LANE_POINTS[2]),
    )
    .unwrap()
    .pan_pixels(1.37, 100)
    .unwrap();
    let mut sequence = 0_u64;
    render_gpu_export(
        &gpu.device,
        &gpu.queue,
        layout,
        FORMAT,
        wgpu::Color::WHITE,
        renderer,
        |renderer, tile| {
            sequence += 1;
            // The lane does not reach every tile; those stay cleared.
            let Some([x, y, width, height]) = tile.scissor(LANE_POINTS) else {
                return Ok(None);
            };
            let origin = tile.origin_points();
            let camera = AlignmentCamera::from_viewport(
                viewport,
                0.25,
                9.0,
                7.0,
                CpuClipRect {
                    x: f64::from(LANE_POINTS[0]) - origin[0],
                    y: f64::from(LANE_POINTS[1]) - origin[1],
                    width: f64::from(LANE_POINTS[2]),
                    height: f64::from(LANE_POINTS[3]),
                },
            )
            .map_err(|error| error.to_string())?;
            let requests = [AlignmentGpuSceneRequest {
                scene,
                camera_sequence: sequence,
                camera,
                screen_size_points: tile.screen_size_points(),
                scissor: AlignmentGpuScissor {
                    x,
                    y,
                    width,
                    height,
                },
                interaction: &interaction,
            }];
            renderer
                .prepare_with_admission(&gpu.device, &gpu.queue, &requests, ledger, sequence)
                .map(|(frame, _, _)| Some(frame))
                .map_err(|error| error.to_string())
        },
        |renderer, frame, pass| match frame {
            Some(frame) => renderer
                .render(pass, frame)
                .map_err(|error| error.to_string()),
            None => Ok(()),
        },
    )
    .unwrap()
}

#[test]
fn tiled_page_pipeline_export_matches_the_untiled_export_and_the_screen_frame() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&gpu.device, FORMAT);

    // The "screen": the frame at one pixel per point.
    let screen = export_scene(
        &gpu,
        &mut renderer,
        &scene,
        &ledger,
        GpuExportLayout::new(FRAME_POINTS[0], FRAME_POINTS[1], 1.0, 8_192).unwrap(),
    );
    assert!(screen.pixels().any(|pixel| pixel.0 != [255, 255, 255, 255]));

    // 3x in one piece, then cut into 96-pixel tiles (a 5x3 grid).
    let untiled_layout =
        GpuExportLayout::new(FRAME_POINTS[0], FRAME_POINTS[1], 3.0, 8_192).unwrap();
    let tiled_layout = GpuExportLayout::new(FRAME_POINTS[0], FRAME_POINTS[1], 3.0, 96).unwrap();
    assert_eq!(tiled_layout.tile_count(), 15);
    let untiled = export_scene(&gpu, &mut renderer, &scene, &ledger, untiled_layout);
    let tiled = export_scene(&gpu, &mut renderer, &scene, &ledger, tiled_layout);
    assert_eq!(untiled.dimensions(), (480, 288));
    let seams = tiled
        .pixels()
        .zip(untiled.pixels())
        .filter(|(tiled, untiled)| tiled != untiled)
        .count();
    assert_eq!(seams, 0, "tiling must not change a single pixel");

    // The export is the screen frame at three times the density: the centre sample of every
    // 3x3 block is the screen pixel, except where a shape edge passes through the block.
    let (mut same, mut different) = (0_usize, 0_usize);
    for (x, y, pixel) in screen.enumerate_pixels() {
        if untiled.get_pixel(x * 3 + 1, y * 3 + 1) == pixel {
            same += 1;
        } else {
            different += 1;
        }
    }
    assert_eq!(
        different,
        0,
        "{different} of {} centre samples differ",
        same + different
    );
}
