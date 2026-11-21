//! Coverage-pyramid depth lane: GPU pixels against the CPU oracle
//! (`project_alignment_depth_pyramid_lane`).

use rgv_alignment_scene::{
    AllocationLedger, CpuClipRect, CpuSummaryCamera, DepthPyramidLane, DepthPyramidStore,
    DepthPyramidView,
};
use rgv_core::{ContigId, CoverageScaleTransform, GenomicInterval, Viewport};
use rgv_model::{CoverageBin, CoveragePyramidParameters, CoverageTile, CoverageTileId};

use super::{FORMAT, Oracle, Quad, Shade, TestGpu, render_image, test_gpu};
use crate::{
    AlignmentDepthPyramidBand, AlignmentDepthPyramidGpuRenderer, AlignmentDepthPyramidGpuRequest,
    AlignmentGpuScissor,
    palette::{AlignmentPalette, PaletteToken, ThemeId},
    project_alignment_depth_pyramid_lane,
};

const TARGET: [u32; 2] = [300, 120];
/// Off the pixel grid, so lane edges do not sit on pixel centres.
const CLIP: CpuClipRect = CpuClipRect {
    x: 16.4,
    y: 8.3,
    width: 256.0,
    height: 96.0,
};
/// Encloses the clip: the vertex stage clamps to the lane itself.
const SCISSOR: AlignmentGpuScissor = AlignmentGpuScissor {
    x: 16,
    y: 8,
    width: 257,
    height: 97,
};
const CONTIG: ContigId = ContigId::new(0);
const CONTIG_LENGTH: u64 = 248_956_422;

/// 64-bin tiles, so a 256-pixel lane spans several tiles of the target level.
fn parameters() -> CoveragePyramidParameters {
    CoveragePyramidParameters::new(16, 4, 64).unwrap()
}

/// Deterministic, varied statistics: a slow wave for the mean, with dropouts (minimum 0),
/// narrow peaks (maximum far above the mean), flat bins (all three equal) and empty bins.
fn synthetic_tile(level: u8, index: u64) -> CoverageTile {
    let parameters = parameters();
    let id = CoverageTileId::new(CONTIG, level, index);
    let count = parameters.tile_bin_count(id, CONTIG_LENGTH).unwrap();
    let bases = parameters.bin_bases(level).unwrap();
    let (start, end) = parameters.tile_span(id, CONTIG_LENGTH).unwrap();
    let bins = (0..count as u64)
        .map(|bin| {
            let global = index * u64::from(parameters.tile_bins()) + bin;
            let bin_bases = (start + (bin + 1) * bases).min(end) - (start + bin * bases);
            let wave = 20 + ((global * 7) % 23) as u32;
            let (minimum, maximum, mean_numerator) = match global % 9 {
                0 => (0, 0, 0),
                1 => (wave, wave, u64::from(wave) * 4),
                2 => (0, wave + 3, u64::from(wave) * 3),
                3 => (wave / 2, 95, u64::from(wave) * 4 + 2),
                _ => (wave - 6, wave + 9, u64::from(wave) * 4 + global % 4),
            };
            // `mean_numerator / 4` lies between the minimum and the maximum.
            CoverageBin {
                depth_sum: (mean_numerator * bin_bases / 4).clamp(
                    u64::from(minimum) * bin_bases,
                    u64::from(maximum) * bin_bases,
                ),
                minimum_depth: minimum,
                maximum_depth: maximum,
            }
        })
        .collect();
    CoverageTile::from_bins(id, parameters, CONTIG_LENGTH, bins).unwrap()
}

fn camera(start: u64, bases_per_pixel: f64, pan_pixels: f64) -> CpuSummaryCamera {
    let span = (bases_per_pixel * CLIP.width).round() as u64;
    CpuSummaryCamera::from_viewport(
        Viewport::new(
            GenomicInterval::new(CONTIG, start, start + span).unwrap(),
            CLIP.width,
        )
        .unwrap()
        .pan_pixels(pan_pixels, CONTIG_LENGTH)
        .unwrap(),
        CLIP,
    )
    .unwrap()
}

fn view(camera: CpuSummaryCamera) -> DepthPyramidView {
    let interval = camera.interval();
    DepthPyramidView {
        contig: CONTIG,
        contig_length: CONTIG_LENGTH,
        start: interval.start(),
        end: interval.end(),
        bases_per_pixel: camera.bases_per_pixel(),
    }
}

fn render(
    gpu: &TestGpu,
    renderer: &mut AlignmentDepthPyramidGpuRenderer,
    lane: &DepthPyramidLane,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> (
    image::RgbaImage,
    crate::AlignmentDepthPyramidGpuPrepareStats,
) {
    let (frame, stats) = renderer
        .prepare(
            &gpu.device,
            &gpu.queue,
            &[AlignmentDepthPyramidGpuRequest {
                lane,
                camera,
                screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
                scissor: SCISSOR,
                scale_maximum,
                transform,
            }],
        )
        .unwrap();
    assert_eq!(frame.draw_count(), stats.draws);
    (
        render_image(gpu, TARGET, |pass| renderer.render(pass, &frame)),
        stats,
    )
}

/// Paints the oracle's bands; returns `(bands, bins)` painted.
fn paint(
    oracle: &mut Oracle,
    palette: &AlignmentPalette,
    lane: &DepthPyramidLane,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
) -> (usize, usize) {
    let depth = palette.color(PaletteToken::SUMMARY_DEPTH);
    let bands = project_alignment_depth_pyramid_lane(lane, camera, scale_maximum, transform);
    let mut bins = std::collections::BTreeSet::new();
    for band in &bands {
        bins.insert(band.start);
        let quad = Quad {
            x: band.bounds.x,
            y: band.bounds.y,
            width: band.bounds.width,
            height: band.bounds.height,
        };
        let colour = [
            f64::from(depth[0]),
            f64::from(depth[1]),
            f64::from(depth[2]),
            f64::from(depth[3] * band.band.alpha()),
        ];
        oracle.paint(
            quad,
            [SCISSOR.x, SCISSOR.y, SCISSOR.width, SCISSOR.height],
            |x, y| match quad.classify(x, y) {
                Some(true) => Shade::Colour(colour),
                Some(false) => Shade::Outside,
                None => Shade::Uncertain,
            },
        );
    }
    (bands.len(), bins.len())
}

fn store_with(tiles: impl IntoIterator<Item = CoverageTile>) -> DepthPyramidStore {
    let ledger = AllocationLedger::new(64 << 20, 0);
    let mut store = DepthPyramidStore::new(parameters(), ledger, 32 << 20);
    for tile in tiles {
        store.insert(tile).unwrap();
    }
    store
}

#[test]
fn depth_pyramid_pixels_match_the_cpu_oracle_at_the_target_level() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
    // 100 bp per pixel: level 2 (256 bp bins, 2.56 pixels each). The camera sits 200 Mbp into
    // the contig and is panned by a fraction of a pixel, so neither the page origin nor the
    // bin edges are on the pixel grid.
    let camera = camera(200_000_000, 100.0, 0.37);
    let level = parameters().level_for_bases_per_pixel(camera.bases_per_pixel());
    assert_eq!(level, 2);
    let tile_bases = parameters().tile_bases(level).unwrap();
    let first = 200_000_000 / tile_bases - 1;
    let mut store = store_with((first..first + 5).map(|index| synthetic_tile(level, index)));
    let lane = store.lane(&view(camera));
    assert!(lane.is_exact_at_target());

    for (scale_maximum, transform) in [
        (60, CoverageScaleTransform::Linear),
        (100, CoverageScaleTransform::SquareRoot),
        // Depths above the scale saturate at the lane's top.
        (30, CoverageScaleTransform::Linear),
    ] {
        let (image, stats) = render(&gpu, &mut renderer, &lane, camera, scale_maximum, transform);
        let mut oracle = Oracle::new(TARGET);
        let (bands, bins) = paint(
            &mut oracle,
            renderer.palette(),
            &lane,
            camera,
            scale_maximum,
            transform,
        );
        // Every bin the camera sees is drawn; all three band kinds occur.
        assert!((90..=110).contains(&bins), "{bins}");
        assert!(bands > bins * 2);
        let label = format!("depth pyramid ({transform:?}, maximum {scale_maximum})");
        let comparison = oracle.assert_matches(&image, &label);
        eprintln!(
            "{label} on {}: {bins} bins, {bands} bands, {} draws, {} pixels compared, {} \
             painted, {} boundary pixels excluded",
            gpu.description,
            stats.draws,
            comparison.compared,
            comparison.painted,
            comparison.uncertain
        );
    }
    // Pages are uploaded once per tile and reused by every later camera.
    assert!(renderer.resident_page_count() >= 2);
    let pages = renderer.resident_page_count();
    let (_, stats) = render(
        &gpu,
        &mut renderer,
        &lane,
        camera,
        60,
        CoverageScaleTransform::Linear,
    );
    assert_eq!(stats.pages_uploaded, 0);
    assert_eq!(stats.uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), pages);
    // Twelve bytes per bin.
    assert_eq!(renderer.resident_page_bytes(), pages * 64 * 12);
}

#[test]
fn depth_pyramid_pixels_match_the_oracle_while_levels_are_mixed_and_after_a_pan() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
    // Target level 3 (1,024 bp bins) at 500 bp per pixel, near the contig start. Only some
    // target tiles are resident; a level-4 ancestor stands in for the rest (coarse first).
    let camera_a = camera(131_072, 500.0, -0.61);
    assert_eq!(
        parameters().level_for_bases_per_pixel(camera_a.bases_per_pixel()),
        3
    );
    let tile_bases = parameters().tile_bases(3).unwrap();
    assert_eq!(tile_bases, 65_536);
    let mut store = store_with([
        synthetic_tile(4, 0),
        synthetic_tile(3, 2),
        // Tile 3 of level 3 is missing: the ancestor shows through there.
        synthetic_tile(3, 4),
    ]);
    let lane = store.lane(&view(camera_a));
    assert!(lane.covers_visible() && !lane.is_exact_at_target());
    let levels = lane
        .segments()
        .iter()
        .map(|segment| segment.tile.tile().id().level)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(levels.into_iter().collect::<Vec<_>>(), [3, 4]);

    let dark = AlignmentPalette::for_theme(ThemeId::Dark);
    for (palette, camera) in [
        (AlignmentPalette::default(), camera_a),
        // A pan and a theme switch reuse the resident pages.
        (dark, camera(140_000, 500.0, 17.25)),
    ] {
        renderer.set_palette(&gpu.queue, &palette);
        let lane = store.lane(&view(camera));
        let (image, stats) = render(
            &gpu,
            &mut renderer,
            &lane,
            camera,
            80,
            CoverageScaleTransform::Linear,
        );
        let mut oracle = Oracle::new(TARGET);
        let (bands, bins) = paint(
            &mut oracle,
            &palette,
            &lane,
            camera,
            80,
            CoverageScaleTransform::Linear,
        );
        assert!(bins > 60 && bands > bins);
        let comparison = oracle.assert_matches(&image, "depth pyramid (mixed levels)");
        eprintln!(
            "depth pyramid (mixed levels) on {}: {bins} bins, {bands} bands, {} draws, {} \
             uploads, {} pixels compared, {} painted, {} boundary pixels excluded",
            gpu.description,
            stats.draws,
            stats.pages_uploaded,
            comparison.compared,
            comparison.painted,
            comparison.uncertain
        );
    }
    assert_eq!(renderer.resident_page_count(), 3);
}

#[test]
fn a_narrow_peak_and_a_dropout_are_drawn_at_whole_chromosome_zoom() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
    // The whole 249 Mbp chromosome on 256 pixels: 972 kbp per pixel, level 8 (1 Mbp bins).
    let camera = CpuSummaryCamera::new(
        GenomicInterval::new(CONTIG, 0, CONTIG_LENGTH).unwrap(),
        CLIP,
    )
    .unwrap();
    let level = parameters()
        .level_for_bases_per_pixel(camera.bases_per_pixel())
        .min(parameters().root_level(CONTIG_LENGTH));
    assert_eq!(level, 8);
    let bases = parameters().bin_bases(level).unwrap();
    // Flat 30x everywhere; bin 100 holds a one-base 3,000x spike and bin 180 a one-base hole.
    let mut tiles = Vec::new();
    for index in parameters().tile_range(level, 0, CONTIG_LENGTH, CONTIG_LENGTH) {
        let id = CoverageTileId::new(CONTIG, level, index);
        let count = parameters().tile_bin_count(id, CONTIG_LENGTH).unwrap();
        let (start, end) = parameters().tile_span(id, CONTIG_LENGTH).unwrap();
        let bins = (0..count as u64)
            .map(|bin| {
                let global = index * 64 + bin;
                let bin_bases = (start + (bin + 1) * bases).min(end) - (start + bin * bases);
                let (minimum, maximum, delta) = match global {
                    100 => (30, 3_000, 2_970_i64),
                    180 => (0, 30, -30),
                    _ => (30, 30, 0),
                };
                CoverageBin {
                    depth_sum: (30 * bin_bases).saturating_add_signed(delta),
                    minimum_depth: minimum,
                    maximum_depth: maximum,
                }
            })
            .collect();
        tiles.push(CoverageTile::from_bins(id, parameters(), CONTIG_LENGTH, bins).unwrap());
    }
    let mut store = store_with(tiles);
    let lane = store.lane(&view(camera));
    assert!(lane.is_exact_at_target());
    assert_eq!(lane.maximum_depth(0, CONTIG_LENGTH), 3_000);

    let transform = CoverageScaleTransform::SquareRoot;
    let (image, _) = render(&gpu, &mut renderer, &lane, camera, 3_000, transform);
    let mut oracle = Oracle::new(TARGET);
    let (bands, bins) = paint(
        &mut oracle,
        renderer.palette(),
        &lane,
        camera,
        3_000,
        transform,
    );
    assert_eq!(bins, 238);
    assert!(bands > bins);
    oracle.assert_matches(&image, "depth pyramid (whole chromosome)");

    // Read the picture itself: the column of bin 100 is painted to the top of the lane (the
    // spike's envelope), its neighbours only to the 30x line; the column of bin 180 has the
    // light "minimum..mean" tone all the way down to the baseline, its neighbours are solid.
    let column = |bin: u64| {
        let centre = CLIP.x + (bin as f64 + 0.5) * bases as f64 / camera.bases_per_pixel();
        centre.floor() as u32
    };
    let pixel = |x: u32, y: u32| image.get_pixel(x, y).0;
    let white = [255, 255, 255, 255];
    let top = (CLIP.y + 2.0) as u32;
    let bottom = (CLIP.y + CLIP.height - 2.0) as u32;
    assert_ne!(pixel(column(100), top), white, "the spike reaches the top");
    assert_eq!(pixel(column(98), top), white);
    assert_eq!(pixel(column(102), top), white);
    let solid = pixel(column(178), bottom);
    assert_ne!(solid, white);
    assert_eq!(pixel(column(182), bottom), solid);
    let dropout = pixel(column(180), bottom);
    assert_ne!(dropout, white);
    assert!(
        dropout[0] > solid[0],
        "the dropout column is lighter at the baseline: {dropout:?} against {solid:?}"
    );
}

#[test]
fn bands_of_one_bin_stack_exactly_and_bins_never_overlap() {
    let camera = camera(200_000_000, 100.0, 0.37);
    let level = 2;
    let tile_bases = parameters().tile_bases(level).unwrap();
    let first = 200_000_000 / tile_bases;
    let mut store = store_with((first..first + 3).map(|index| synthetic_tile(level, index)));
    let lane = store.lane(&view(camera));
    let bands =
        project_alignment_depth_pyramid_lane(&lane, camera, 60, CoverageScaleTransform::Linear);
    assert!(!bands.is_empty());
    let baseline = CLIP.y + CLIP.height;
    let mut by_bin = std::collections::BTreeMap::<u64, Vec<_>>::new();
    for band in &bands {
        // Inside the lane.
        assert!(band.bounds.x >= CLIP.x - 1e-9);
        assert!(band.bounds.x + band.bounds.width <= CLIP.x + CLIP.width + 1e-9);
        assert!(band.bounds.y >= CLIP.y - 1e-9);
        assert!(band.bounds.height > 0.0 && band.bounds.width > 0.0);
        by_bin.entry(band.start).or_default().push(*band);
    }
    let mut previous_right = f64::NEG_INFINITY;
    for bin in by_bin.values() {
        // Draw order is minimum, mean, envelope; each starts where the one below ends.
        let mut cursor = baseline;
        let mut last = None;
        for band in bin {
            assert!(last < Some(band.band as u8));
            last = Some(band.band as u8);
            assert!((band.bounds.y + band.bounds.height - cursor).abs() < 1e-9);
            cursor = band.bounds.y;
            assert!((band.bounds.x - bin[0].bounds.x).abs() < 1e-12);
        }
        // Bins abut and never overlap horizontally.
        assert!(bin[0].bounds.x >= previous_right - 1e-9);
        previous_right = bin[0].bounds.x + bin[0].bounds.width;
    }
    assert!(
        bands
            .iter()
            .any(|band| band.band == AlignmentDepthPyramidBand::Envelope)
    );
    // A bin is at least a pixel wide at its target level.
    let widest = by_bin
        .values()
        .map(|bin| bin[0].bounds.width)
        .fold(0.0, f64::max);
    assert!((2.5..=2.6).contains(&widest), "{widest}");
}

#[test]
fn gpu_pages_are_charged_to_the_shared_ledger_exactly_and_released_with_the_pages() {
    use rgv_alignment_scene::AllocationOwner;
    let Some(gpu) = test_gpu() else {
        return;
    };
    // The tiles themselves are charged to the store's own ledger (`store_with`), so this one
    // sees only what the renderer charges.
    let ledger = AllocationLedger::new(64 << 20, 0);
    let gpu_charged = || ledger.owner_usage(AllocationOwner::Gpu).unwrap().charged;
    let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
    renderer.set_ledger(ledger.clone());
    assert_eq!(gpu_charged(), 0);

    let level = 2;
    let tile_bases = parameters().tile_bases(level).unwrap();
    let first = 200_000_000 / tile_bases;
    let mut store = store_with((first..first + 6).map(|index| synthetic_tile(level, index)));
    let camera_a = camera(200_000_000, 100.0, 0.37);
    let lane = store.lane(&view(camera_a));
    let transform = CoverageScaleTransform::Linear;
    let (_, stats) = render(&gpu, &mut renderer, &lane, camera_a, 60, transform);
    assert!(stats.pages_uploaded >= 2);
    assert_eq!(stats.pages_refused, 0);
    // Every byte uploaded is charged, plus the page table, and nothing else is.
    let page_bytes = renderer.resident_page_bytes();
    assert_eq!(page_bytes, stats.uploaded_bytes);
    assert_eq!(page_bytes, renderer.resident_page_count() * 64 * 12);
    let charged = gpu_charged();
    assert_eq!(charged, renderer.ledger_charged_bytes().unwrap());
    assert!(charged > page_bytes, "the page table is charged too");
    let table_bytes = charged - page_bytes;

    // Drawing the same pages again charges nothing more.
    let (_, again) = render(&gpu, &mut renderer, &lane, camera_a, 60, transform);
    assert_eq!(
        (again.pages_uploaded, again.pages_reused),
        (0, stats.pages_uploaded)
    );
    assert_eq!(gpu_charged(), charged);

    // Evicted pages give their charge back: with a byte cap of zero only the pages of the
    // current frame stay, after a pan onto other tiles.
    renderer.set_page_byte_limit(0);
    let camera_b = camera(200_000_000 + 4 * tile_bases, 100.0, 0.0);
    let lane_b = store.lane(&view(camera_b));
    let (_, moved) = render(&gpu, &mut renderer, &lane_b, camera_b, 60, transform);
    assert!(moved.pages_uploaded >= 1 && moved.pages_evicted >= 1);
    assert_eq!(
        renderer.resident_page_bytes(),
        renderer.resident_page_count() * 64 * 12
    );
    assert_eq!(gpu_charged(), renderer.ledger_charged_bytes().unwrap());
    // What remains charged is the resident pages and a page table (which may have grown
    // while old and new pages were resident together), not the evicted pages.
    assert!(gpu_charged() - renderer.resident_page_bytes() <= 4 * table_bytes);

    // Dropping the device resources releases everything.
    renderer.discard_device_resources();
    assert_eq!(gpu_charged(), 0);
    assert_eq!(renderer.resident_page_count(), 0);

    // A ledger with room for one page but not for the frame: the pages that fit are drawn,
    // the rest are refused and reported, and the budget is never exceeded.
    let budget = table_bytes + 64 * 12 + 64;
    let small = AllocationLedger::new(budget, 0);
    let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
    renderer.set_ledger(small.clone());
    let (frame, tight) = renderer
        .prepare(
            &gpu.device,
            &gpu.queue,
            &[AlignmentDepthPyramidGpuRequest {
                lane: &lane,
                camera: camera_a,
                screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
                scissor: SCISSOR,
                scale_maximum: 60,
                transform,
            }],
        )
        .unwrap();
    assert!(tight.pages_refused >= 1, "{tight:?}");
    assert_eq!(
        tight.pages_uploaded + tight.pages_refused,
        stats.pages_uploaded
    );
    assert_eq!(frame.draw_count(), tight.draws);
    assert!(tight.draws < stats.draws);
    let used = small.owner_usage(AllocationOwner::Gpu).unwrap().charged;
    assert_eq!(used, renderer.ledger_charged_bytes().unwrap());
    assert!(used <= budget);
}

#[test]
fn a_refused_page_is_reported_for_the_lane_that_needed_it_and_no_other() {
    use rgv_alignment_scene::AllocationOwner;
    let Some(gpu) = test_gpu() else {
        return;
    };
    let level = 2;
    let tile_bases = parameters().tile_bases(level).unwrap();
    let first = 200_000_000 / tile_bases;
    let mut store = store_with((first..first + 12).map(|index| synthetic_tile(level, index)));
    // Two lanes of one frame over disjoint tiles.
    let camera_a = camera(200_000_000, 100.0, 0.37);
    let camera_b = camera(200_000_000 + 8 * tile_bases, 100.0, 0.0);
    let lane_a = store.lane(&view(camera_a));
    let lane_b = store.lane(&view(camera_b));
    let transform = CoverageScaleTransform::Linear;
    let request = |lane, camera| AlignmentDepthPyramidGpuRequest {
        lane,
        camera,
        screen_size_points: [TARGET[0] as f32, TARGET[1] as f32],
        scissor: SCISSOR,
        scale_maximum: 60,
        transform,
    };
    for (first, second) in [
        ((&lane_a, camera_a), (&lane_b, camera_b)),
        ((&lane_b, camera_b), (&lane_a, camera_a)),
    ] {
        // What the first lane alone charges: its pages and the page table.
        let roomy = AllocationLedger::new(64 << 20, 0);
        let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
        renderer.set_ledger(roomy.clone());
        let (frame, alone) = renderer
            .prepare(&gpu.device, &gpu.queue, &[request(first.0, first.1)])
            .unwrap();
        assert!(alone.pages_uploaded >= 1);
        assert_eq!(frame.pages_refused_by_request(), [0]);
        let first_charge = roomy.owner_usage(AllocationOwner::Gpu).unwrap().charged;
        let (_, both) = renderer
            .prepare(
                &gpu.device,
                &gpu.queue,
                &[request(first.0, first.1), request(second.0, second.1)],
            )
            .unwrap();
        assert!(both.pages_uploaded >= 1, "the lanes share no page");

        // Room for exactly that and not one page more: the second lane is refused, and only
        // the second lane is reported refused.
        let tight = AllocationLedger::new(first_charge + 64, 0);
        let mut renderer = AlignmentDepthPyramidGpuRenderer::new(&gpu.device, FORMAT);
        renderer.set_ledger(tight);
        let (frame, stats) = renderer
            .prepare(
                &gpu.device,
                &gpu.queue,
                &[request(first.0, first.1), request(second.0, second.1)],
            )
            .unwrap();
        assert_eq!(stats.pages_uploaded, alone.pages_uploaded);
        assert_eq!(stats.pages_refused, both.pages_uploaded);
        assert_eq!(
            frame.pages_refused_by_request(),
            [0, both.pages_uploaded],
            "{stats:?}"
        );
        assert_eq!(frame.request_pages_refused(0), 0);
        assert_eq!(frame.request_pages_refused(1), both.pages_uploaded);
        assert_eq!(frame.request_pages_refused(2), 0);
        assert_eq!(
            frame.pages_refused_by_request().iter().sum::<usize>(),
            stats.pages_refused
        );
        // The first lane is drawn whole.
        assert_eq!(frame.draw_count(), alone.draws);
    }
}
