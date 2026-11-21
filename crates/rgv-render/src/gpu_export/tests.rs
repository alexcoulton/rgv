use super::*;

#[test]
fn layout_scales_the_frame_and_obeys_the_cpu_exporter_limits() {
    let layout = GpuExportLayout::new(400, 200, 2.0, 8_192).unwrap();
    assert_eq!(layout.size_pixels(), [800, 400]);
    assert!(!layout.is_tiled());
    assert_eq!(layout.tile_count(), 1);
    assert_eq!(layout.tile_extent(), [800, 400]);
    // Same rounding as the CPU exporter.
    assert_eq!(
        GpuExportLayout::new(3, 3, 1.5, 8_192)
            .unwrap()
            .size_pixels(),
        [5, 5]
    );
    assert_eq!(
        GpuExportLayout::new(0, 0, 1.0, 8_192)
            .unwrap()
            .size_pixels(),
        [1, 1]
    );
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(matches!(
            GpuExportLayout::new(8, 8, scale, 8_192),
            Err(GpuExportError::InvalidScale)
        ));
    }
    assert!(matches!(
        GpuExportLayout::new(crate::MAX_EXPORT_PNG_EDGE, 8, 2.0, 8_192),
        Err(GpuExportError::TooLarge { .. })
    ));
    assert!(matches!(
        GpuExportLayout::new(8, 8, 1.0, 0),
        Err(GpuExportError::InvalidTileEdge)
    ));
}

#[test]
fn tiles_cover_every_pixel_exactly_once() {
    let layout = GpuExportLayout::new(1_000, 300, 2.5, 1_024).unwrap();
    assert_eq!(layout.size_pixels(), [2_500, 750]);
    assert!(layout.is_tiled());
    assert_eq!(layout.tile_extent(), [1_024, 750]);
    let tiles = layout.tiles();
    assert_eq!(tiles.len(), 3);
    assert_eq!(tiles.len(), layout.tile_count());
    let mut covered = vec![0_u8; 2_500 * 750];
    for (index, tile) in tiles.iter().enumerate() {
        assert_eq!(tile.index, index);
        assert!(tile.size_pixels[0] <= 1_024 && tile.size_pixels[1] <= 1_024);
        for y in tile.origin_pixels[1]..tile.origin_pixels[1] + tile.size_pixels[1] {
            for x in tile.origin_pixels[0]..tile.origin_pixels[0] + tile.size_pixels[0] {
                covered[(y * 2_500 + x) as usize] += 1;
            }
        }
    }
    assert!(covered.iter().all(|count| *count == 1));

    // Both axes tiled, with a ragged last row and column.
    let grid = GpuExportLayout::new(100, 70, 1.0, 32).unwrap();
    assert_eq!(grid.tile_count(), 4 * 3);
    let last = *grid.tiles().last().unwrap();
    assert_eq!(last.origin_pixels, [96, 64]);
    assert_eq!(last.size_pixels, [4, 6]);
}

#[test]
fn tile_transform_maps_frame_points_to_tile_pixels() {
    let layout = GpuExportLayout::new(1_000, 300, 2.5, 1_024).unwrap();
    let tiles = layout.tiles();
    let second = tiles[1];
    assert_eq!(second.origin_pixels, [1_024, 0]);
    assert!((second.origin_points()[0] - 409.6).abs() < 1.0e-9);
    assert!(second.origin_points()[1].abs() < f64::EPSILON);
    let size = second.screen_size_points();
    assert!((size[0] - 409.6).abs() < 1.0e-4);
    assert!((size[1] - 300.0).abs() < 1.0e-4);

    // A lane spanning the first two tiles: each tile gets its own part.
    let lane = [300.0, 20.0, 200.0, 100.0];
    assert_eq!(tiles[0].scissor(lane), Some([750, 50, 274, 250]));
    assert_eq!(second.scissor(lane), Some([0, 50, 226, 250]));
    assert_eq!(tiles[2].scissor(lane), None);
    // Untiled, the scissor is the lane rectangle times the scale, like a window scissor.
    let whole = GpuExportLayout::new(1_000, 300, 2.0, 8_192)
        .unwrap()
        .tiles()[0];
    assert!(
        whole
            .origin_points()
            .iter()
            .all(|origin| origin.abs() < f64::EPSILON)
    );
    assert!((whole.screen_size_points()[0] - 1_000.0).abs() < 1.0e-4);
    assert!((whole.screen_size_points()[1] - 300.0).abs() < 1.0e-4);
    assert_eq!(whole.scissor(lane), Some([600, 40, 400, 200]));
    assert_eq!(whole.scissor([10.0, 10.0, 0.0, 5.0]), None);
    assert_eq!(
        whole.scissor([-50.0, -50.0, 60.0, 60.0]),
        Some([0, 0, 20, 20])
    );
}

#[test]
fn readback_rows_are_padded_to_the_copy_alignment() {
    assert_eq!(padded_bytes_per_row(1), 256);
    assert_eq!(padded_bytes_per_row(64), 256);
    assert_eq!(padded_bytes_per_row(65), 512);
    assert_eq!(padded_bytes_per_row(300), 1_280);
}

#[test]
fn export_without_a_device_is_the_cpu_export() {
    let directory = tempfile::tempdir().unwrap();
    let display = DisplayList::synthetic_coverage_and_reads_at(320.0, 24, 1_000, 8.0);
    let fallback = directory.path().join("fallback.png");
    let cpu = directory.path().join("cpu.png");
    let backend = export_png_scaled_prefer_gpu(None, &display, 320, 240, 2.0, &fallback).unwrap();
    assert_eq!(backend, PngExportBackend::Cpu { gpu_failure: None });
    crate::export_png_scaled(&display, 320, 240, 2.0, &cpu).unwrap();
    assert_eq!(
        std::fs::read(&fallback).unwrap(),
        std::fs::read(&cpu).unwrap()
    );
    assert!(export_png_scaled_prefer_gpu(None, &display, 320, 240, 0.0, &cpu).is_err());
}
