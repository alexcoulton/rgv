//! GPU readback goldens of the world-space track tile pipeline. Scenes are produced by the
//! real world-space layouts from model tiles, uploaded as pages, drawn, read back and compared
//! with an image rasterised on the CPU from `project_track_instance` and
//! `track_fragment_coverage`.

#![allow(clippy::float_cmp, clippy::too_many_lines)]

use std::sync::Arc;

use rgv_alignment_scene::AllocationLedger;
use rgv_layout::{
    hit_test_track_scene, lay_out_bin_tile, lay_out_feature_tile, lay_out_interaction_tile,
    lay_out_variant_tile,
};
use rgv_model::{
    BinSemantics, BinTile, FeatureRowAxis, FeatureTile, GenotypeCall, GenotypeMatrix,
    InteractionTile, Strand, TileBin, TileFeature, TileFeatureBlock, TileInteraction, TileVariant,
    TileVariantKind, TrackColor, TrackRecordId, TrackTile, TrackTileKey, TrackTileLod,
    TrackTilePayload, TrackTileProvenance, TrackTileScene,
};

use super::{
    FORMAT, Oracle, Quad, Shade, TestGpu, painted_columns, render_image, shade_with_probe, test_gpu,
};
use crate::{
    TRACK_TILE_INSTANCE_BYTES, TrackBinStyle, TrackLaneDraw, TrackLanePage, TrackLaneProjection,
    TrackPalette, TrackTileFrame, TrackTilePageKey, TrackTileRenderError, TrackTileRenderer,
    palette::ThemeId, project_track_instance, track_fragment_color, track_fragment_coverage,
};

fn tile(key: TrackTileKey, payload: TrackTilePayload) -> TrackTile {
    TrackTile {
        contig: "chr1".to_owned(),
        key,
        lod: TrackTileLod::Detail,
        payload,
        provenance: TrackTileProvenance::default(),
    }
}

fn feature(identity: u64, start: u64, end: u64, strand: Strand, row: u32) -> TileFeature {
    TileFeature {
        id: TrackRecordId(identity),
        start,
        end,
        strand,
        blocks: Vec::new(),
        name: None,
        score: None,
        color: None,
        row_hint: Some(row),
        labelable: false,
    }
}

fn feature_scene(key: TrackTileKey, features: Vec<TileFeature>) -> TrackTileScene {
    lay_out_feature_tile(&tile(
        key,
        TrackTilePayload::Features(FeatureTile {
            features,
            rows: FeatureRowAxis::Packed,
        }),
    ))
    .unwrap()
}

fn page_key(track: u64, scene: &TrackTileScene) -> TrackTilePageKey {
    TrackTilePageKey {
        track,
        tile: scene.key,
        settings_digest: 0,
    }
}

fn lane(rect: [f32; 4], camera_start: f64, points_per_base: f64) -> TrackLaneProjection {
    TrackLaneProjection {
        rect,
        camera_start,
        points_per_base,
        row_pitch: 14.0,
        row_height: 10.0,
        row_offset: 3.0,
        value_range: (0.0, 12.0),
        bin_style: TrackBinStyle::Bars,
    }
}

struct LaneScene<'scene> {
    projection: TrackLaneProjection,
    scene: &'scene TrackTileScene,
    track: u64,
    selected: Option<u32>,
    hovered: Option<u32>,
}

/// Paints one scene the way the pipeline does: every instance, in order, inside the tile's
/// scissor.
fn paint_scene(
    oracle: &mut Oracle,
    lane: &LaneScene<'_>,
    palette: &TrackPalette,
    scale: f64,
    target: [u32; 2],
) {
    let Some(scissor) = lane.projection.tile_scissor(lane.scene.key, scale, target) else {
        return;
    };
    for instance in &lane.scene.instances {
        let projected = project_track_instance(instance, lane.scene.key.start(), &lane.projection);
        let base = palette.resolve(projected.color);
        let selected = projected.record.is_some() && projected.record == lane.selected;
        let hovered = projected.record.is_some() && projected.record == lane.hovered;
        let quad = Quad {
            x: projected.quad[0] * scale,
            y: projected.quad[1] * scale,
            width: (projected.quad[2] - projected.quad[0]) * scale,
            height: (projected.quad[3] - projected.quad[1]) * scale,
        };
        oracle.paint(quad, scissor, |x, y| match quad.classify(x, y) {
            Some(false) => Shade::Outside,
            None => Shade::Uncertain,
            Some(true) => shade_with_probe(x, y, |x, y| {
                let coverage = track_fragment_coverage(
                    projected.shape,
                    [x / scale, y / scale],
                    projected.bounds,
                )?;
                Some(track_fragment_color(base, coverage, selected, hovered))
            }),
        });
    }
}

/// Admits, prepares and renders `lanes`, and returns the GPU image and the oracle.
fn render_lanes(
    gpu: &TestGpu,
    renderer: &mut TrackTileRenderer,
    lanes: &[LaneScene<'_>],
    screen: [u32; 2],
    scale: f64,
) -> (image::RgbaImage, Oracle) {
    let size = [
        (f64::from(screen[0]) * scale) as u32,
        (f64::from(screen[1]) * scale) as u32,
    ];
    let keys = lanes
        .iter()
        .map(|lane| page_key(lane.track, lane.scene))
        .collect::<Vec<_>>();
    for lane in lanes {
        renderer
            .admit_page(
                &gpu.device,
                page_key(lane.track, lane.scene),
                lane.scene,
                &keys,
            )
            .unwrap();
    }
    let frame = TrackTileFrame {
        screen_size: [screen[0] as f32, screen[1] as f32],
        scale_factor: scale,
        target_size_pixels: size,
        lanes: lanes
            .iter()
            .map(|lane| TrackLaneDraw {
                projection: lane.projection,
                pages: vec![TrackLanePage {
                    key: page_key(lane.track, lane.scene),
                    selected: lane.selected,
                    hovered: lane.hovered,
                }],
            })
            .collect(),
    };
    let report = renderer.prepare(&gpu.device, &gpu.queue, &frame);
    assert!(report.missing_pages.is_empty(), "{report:?}");
    let image = render_image(gpu, size, |pass| {
        renderer.render(pass);
        Ok::<(), std::convert::Infallible>(())
    });
    let mut oracle = Oracle::new(size);
    let palette = *renderer.palette();
    for lane in lanes {
        paint_scene(&mut oracle, lane, &palette, scale, size);
    }
    (image, oracle)
}

fn gallery_scenes(key: TrackTileKey) -> [TrackTileScene; 4] {
    let base = key.start();
    let mut transcript = feature(1, base + 40, base + 260, Strand::Forward, 0);
    transcript.blocks = vec![
        TileFeatureBlock {
            start: base + 40,
            end: base + 60,
            thick: false,
        },
        TileFeatureBlock {
            start: base + 60,
            end: base + 95,
            thick: true,
        },
        TileFeatureBlock {
            start: base + 150,
            end: base + 190,
            thick: true,
        },
        TileFeatureBlock {
            start: base + 230,
            end: base + 260,
            thick: true,
        },
    ];
    let mut reverse_transcript = feature(2, base + 20, base + 200, Strand::Reverse, 1);
    reverse_transcript.blocks = vec![
        TileFeatureBlock {
            start: base + 20,
            end: base + 70,
            thick: true,
        },
        TileFeatureBlock {
            start: base + 160,
            end: base + 200,
            thick: false,
        },
    ];
    let mut inline = feature(6, base + 130, base + 215, Strand::Unstranded, 2);
    inline.color = Some(TrackColor::Inline([200, 120, 30, 140]));
    let features = feature_scene(
        key,
        vec![
            transcript,
            reverse_transcript,
            feature(3, base + 30, base + 110, Strand::Reverse, 2),
            inline,
            // A translucent shape over another one: blending order matters.
            feature(4, base + 222, base + 262, Strand::Forward, 2),
            // One base at 0.9 points per base: drawn at the one-point minimum.
            feature(5, base + 280, base + 281, Strand::Unstranded, 2),
        ],
    );

    let mut bins = vec![TileBin::EMPTY; 300];
    for (index, bin) in bins.iter_mut().enumerate().take(280).skip(8) {
        let phase = index as f32 * 0.21;
        let mean = 5.0 + 4.0 * phase.sin();
        *bin = if index % 9 < 5 {
            TileBin {
                minimum: mean - 1.5,
                mean,
                maximum: mean + 2.0 + phase.cos(),
            }
        } else {
            TileBin {
                minimum: mean,
                mean,
                maximum: mean,
            }
        };
    }
    let bins = lay_out_bin_tile(&tile(
        key,
        TrackTilePayload::Bins(BinTile {
            bin_width: 1,
            bins,
            semantics: BinSemantics::Signal,
        }),
    ))
    .unwrap();

    let samples: Arc<[String]> = (0..4).map(|sample| format!("s{sample}")).collect();
    let positions = [50_u64, 51, 90, 140, 141, 142, 200, 255];
    let mut matrix = GenotypeMatrix::new(samples, positions.len());
    for variant in 0..positions.len() {
        for sample in 0..4 {
            matrix.set(
                variant,
                sample,
                match (variant * 3 + sample * 5) % 4 {
                    0 => GenotypeCall::HomRef,
                    1 => GenotypeCall::Het,
                    2 => GenotypeCall::HomAlt,
                    _ => GenotypeCall::NoCall,
                },
            );
        }
    }
    let variants = lay_out_variant_tile(&tile(
        key,
        TrackTilePayload::Variants(rgv_model::VariantTile {
            variants: positions
                .iter()
                .enumerate()
                .map(|(index, position)| TileVariant {
                    id: TrackRecordId(100 + index as u64),
                    start: base + position,
                    end: base + position + if index == 3 { 30 } else { 1 },
                    kind: [
                        TileVariantKind::Snv,
                        TileVariantKind::Insertion,
                        TileVariantKind::Deletion,
                        TileVariantKind::Structural,
                        TileVariantKind::Other,
                    ][index % 5],
                    name: None,
                    quality: None,
                })
                .collect(),
            genotypes: Some(matrix),
        }),
    ))
    .unwrap();

    let interaction = |identity, source: u64, target: u64, color| TileInteraction {
        id: TrackRecordId(identity),
        source: (base + source, base + source + 12),
        target: Some((base + target, base + target + 12)),
        score: None,
        color,
        name: None,
    };
    let interactions = lay_out_interaction_tile(&tile(
        key,
        TrackTilePayload::Interactions(InteractionTile {
            interactions: vec![
                interaction(200, 30, 250, None),
                interaction(201, 90, 170, Some(TrackColor::Inline([30, 140, 90, 255]))),
                interaction(202, 120, 1_400, None),
            ],
        }),
    ))
    .unwrap();
    [features, bins, variants, interactions]
}

#[test]
fn track_tile_pipeline_pixels_match_the_cpu_oracle_for_every_content_kind() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let key = TrackTileKey {
        level: 0,
        index: 976,
    };
    let [features, bins, variants, interactions] = gallery_scenes(key);
    let screen = [300_u32, 280];
    let camera = key.start() as f64 + 10.37;
    for (scale, theme) in [
        (1.0_f64, ThemeId::Light),
        (2.0, ThemeId::Dark),
        (2.5, ThemeId::LightColorBlind),
    ] {
        let mut renderer = TrackTileRenderer::new(
            &gpu.device,
            FORMAT,
            AllocationLedger::new(64 << 20, 1 << 20),
        );
        renderer.set_palette(&gpu.queue, &TrackPalette::for_theme(theme));
        let mut line_bins = lane([8.0, 106.0, 280.0, 38.0], camera, 0.9);
        line_bins.bin_style = TrackBinStyle::Line;
        let lanes = [
            LaneScene {
                projection: lane([8.0, 6.0, 280.0, 48.0], camera, 0.9),
                scene: &features,
                track: 1,
                selected: features.record_index_of(TrackRecordId(3)),
                hovered: features.record_index_of(TrackRecordId(1)),
            },
            LaneScene {
                projection: lane([8.0, 60.0, 280.0, 40.0], camera, 0.9),
                scene: &bins,
                track: 2,
                selected: None,
                hovered: None,
            },
            LaneScene {
                projection: line_bins,
                scene: &bins,
                track: 2,
                selected: None,
                hovered: None,
            },
            LaneScene {
                projection: lane([8.0, 150.0, 280.0, 74.0], camera - 3.21, 1.7),
                scene: &variants,
                track: 3,
                selected: variants.record_index_of(TrackRecordId(106)),
                hovered: None,
            },
            LaneScene {
                projection: lane([8.0, 230.0, 280.0, 44.0], camera, 0.9),
                scene: &interactions,
                track: 4,
                selected: None,
                hovered: interactions.record_index_of(TrackRecordId(201)),
            },
        ];
        let (image, oracle) = render_lanes(&gpu, &mut renderer, &lanes, screen, scale);
        let comparison = oracle.assert_matches(
            &image,
            &format!("track tile pipeline at {scale}x {theme:?}"),
        );
        eprintln!(
            "track tile pipeline at {scale}x on {}: {} pixels compared, {} painted, {} boundary \
             pixels excluded",
            gpu.description, comparison.compared, comparison.painted, comparison.uncertain
        );
        assert!(comparison.painted > 4_000, "{comparison:?}");
        // Four distinct pages (the two bin lanes share one), charged at exactly 28 bytes each.
        assert_eq!(renderer.page_count(), 4);
        let instances = features.instances.len()
            + bins.instances.len()
            + variants.instances.len()
            + interactions.instances.len();
        assert_eq!(
            renderer.resident_page_bytes().unwrap() - renderer.page_table_bytes(),
            instances * TRACK_TILE_INSTANCE_BYTES
        );
    }
}

#[test]
fn a_shape_crossing_a_tile_seam_is_one_continuous_projection_drawn_once() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let left_key = TrackTileKey {
        level: 0,
        index: 975,
    };
    let right_key = TrackTileKey {
        level: 0,
        index: 976,
    };
    let seam = right_key.start();
    // Translucent, so a pixel drawn by both tiles (or by neither) would show.
    let spanning = |row| {
        let mut spanning = feature(1, seam - 90, seam + 70, Strand::Forward, row);
        spanning.color = Some(TrackColor::Inline([20, 90, 200, 128]));
        spanning.blocks = vec![
            TileFeatureBlock {
                start: seam - 90,
                end: seam - 40,
                thick: true,
            },
            // This block itself straddles the seam.
            TileFeatureBlock {
                start: seam - 12,
                end: seam + 9,
                thick: true,
            },
            TileFeatureBlock {
                start: seam + 40,
                end: seam + 70,
                thick: false,
            },
        ];
        spanning
    };
    let mut plain = feature(2, seam - 30, seam + 30, Strand::Reverse, 1);
    plain.color = Some(TrackColor::Inline([200, 60, 40, 128]));
    let left = feature_scene(left_key, vec![spanning(0), plain.clone()]);
    let right = feature_scene(right_key, vec![spanning(0), plain.clone()]);
    // The same row in both tiles is the engine's job; here it is given.
    let screen = [260_u32, 60];
    for (scale, points_per_base, camera_offset) in [
        (1.0_f64, 1.0, 120.37),
        (2.0, 1.31, 96.2),
        (2.5, 0.77, 150.61),
    ] {
        let camera = seam as f64 - camera_offset;
        let projection = lane([6.0, 8.0, 248.0, 40.0], camera, points_per_base);
        let mut renderer = TrackTileRenderer::new(
            &gpu.device,
            FORMAT,
            AllocationLedger::new(64 << 20, 1 << 20),
        );
        let size = [
            (f64::from(screen[0]) * scale) as u32,
            (f64::from(screen[1]) * scale) as u32,
        ];
        let keys = [page_key(1, &left), page_key(1, &right)];
        renderer
            .admit_page(&gpu.device, keys[0], &left, &keys)
            .unwrap();
        renderer
            .admit_page(&gpu.device, keys[1], &right, &keys)
            .unwrap();
        let report = renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &TrackTileFrame {
                screen_size: [screen[0] as f32, screen[1] as f32],
                scale_factor: scale,
                target_size_pixels: size,
                lanes: vec![TrackLaneDraw {
                    projection,
                    pages: keys
                        .iter()
                        .map(|key| TrackLanePage {
                            key: *key,
                            selected: None,
                            hovered: None,
                        })
                        .collect(),
                }],
            },
        );
        assert_eq!(report.draws, 2);
        let image = render_image(&gpu, size, |pass| {
            renderer.render(pass);
            Ok::<(), std::convert::Infallible>(())
        });
        // The reference is ONE scene holding each shape once, drawn across the whole lane: it
        // knows nothing about tiles. Scissor: the lane's pixels.
        let whole_key = TrackTileKey {
            level: 5,
            index: 30,
        };
        assert!(whole_key.start() <= left_key.start() && whole_key.end() >= right_key.end());
        let whole = feature_scene(whole_key, vec![spanning(0), plain.clone()]);
        let lane_scissor = {
            let left_scissor = projection.tile_scissor(left_key, scale, size).unwrap();
            let right_scissor = projection.tile_scissor(right_key, scale, size).unwrap();
            assert_eq!(left_scissor[0] + left_scissor[2], right_scissor[0]);
            [
                left_scissor[0],
                left_scissor[1],
                left_scissor[2] + right_scissor[2],
                left_scissor[3],
            ]
        };
        let mut oracle = Oracle::new(size);
        let palette = *renderer.palette();
        for instance in &whole.instances {
            let projected = project_track_instance(instance, whole_key.start(), &projection);
            let base = palette.resolve(projected.color);
            let quad = Quad {
                x: projected.quad[0] * scale,
                y: projected.quad[1] * scale,
                width: (projected.quad[2] - projected.quad[0]) * scale,
                height: (projected.quad[3] - projected.quad[1]) * scale,
            };
            oracle.paint(quad, lane_scissor, |x, y| match quad.classify(x, y) {
                Some(false) => Shade::Outside,
                None => Shade::Uncertain,
                Some(true) => shade_with_probe(x, y, |x, y| {
                    let coverage = track_fragment_coverage(
                        projected.shape,
                        [x / scale, y / scale],
                        projected.bounds,
                    )?;
                    Some(track_fragment_color(base, coverage, false, false))
                }),
            });
        }
        let comparison = oracle.assert_matches(
            &image,
            &format!("seam at {scale}x, {points_per_base} points per base"),
        );
        eprintln!(
            "track tile seam at {scale}x on {}: {} compared, {} painted, {} excluded",
            gpu.description, comparison.compared, comparison.painted, comparison.uncertain
        );
        // Both tiles hit-test the spanning feature to the same stable identity at the seam.
        let at = |x_points: f32| projection.scene_point(x_points, 8.0 + 3.0 + 5.0);
        let seam_x = projection.x_of(seam as f64) as f32;
        let tolerance = projection.hit_tolerance_bases();
        let from_left = hit_test_track_scene(&left, at(seam_x - 1.0), tolerance).unwrap();
        let from_right = hit_test_track_scene(&right, at(seam_x + 1.0), tolerance).unwrap();
        assert_eq!(from_left.id, Some(TrackRecordId(1)));
        assert_eq!(from_left.id, from_right.id);
    }
}

#[test]
fn deep_zoom_far_from_the_origin_follows_the_camera_exactly() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    // The last megabase of human chr1, 20 points per base: a single-f32 projection would step
    // by 16 bases here.
    let key = TrackTileKey::containing(0, 248_956_000);
    let base = key.start();
    let scene = feature_scene(
        key,
        vec![
            feature(1, base + 500, base + 503, Strand::Forward, 0),
            feature(2, base + 505, base + 506, Strand::Unstranded, 1),
            // Starts megabases to the left and ends in view: a huge negative local offset.
            feature(
                3,
                base.saturating_sub(40_000_000),
                base + 499,
                Strand::Reverse,
                2,
            ),
        ],
    );
    assert_eq!(scene.instances[2].start, -40_000_000);
    let screen = [240_u32, 60];
    let mut renderer = TrackTileRenderer::new(
        &gpu.device,
        FORMAT,
        AllocationLedger::new(64 << 20, 1 << 20),
    );
    let mut previous_columns = None;
    for step in 0..12 {
        // 0.05 bases = one point per step.
        let camera = base as f64 + 496.013 + f64::from(step) * 0.05;
        let lanes = [LaneScene {
            projection: lane([0.0, 4.0, 240.0, 50.0], camera, 20.0),
            scene: &scene,
            track: 1,
            selected: None,
            hovered: None,
        }];
        let (image, oracle) = render_lanes(&gpu, &mut renderer, &lanes, screen, 1.0);
        oracle.assert_matches(&image, &format!("deep zoom step {step}"));
        // The one-base feature on row 1 is 20 columns wide and moves one column per step.
        let columns = painted_columns(&image, 4 + 3 + 14 + 5).unwrap();
        assert_eq!(columns.1 - columns.0, 20, "step {step}");
        if let Some((previous, _)) = previous_columns {
            assert_eq!(columns.0 + 1, previous, "step {step}");
        }
        previous_columns = Some(columns);
    }
    // Twelve cameras, one upload.
    assert_eq!(renderer.page_count(), 1);
}

#[test]
fn features_narrower_than_a_point_are_drawn_one_point_wide_at_any_zoom() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    // Level 10: 1,024 bases per point. A 30-base feature is 0.03 points.
    let key = TrackTileKey {
        level: 10,
        index: 3,
    };
    let scene = feature_scene(
        key,
        vec![feature(
            1,
            key.start() + 51_200,
            key.start() + 51_230,
            Strand::Unstranded,
            0,
        )],
    );
    let mut renderer = TrackTileRenderer::new(
        &gpu.device,
        FORMAT,
        AllocationLedger::new(64 << 20, 1 << 20),
    );
    for scale in [1.0_f64, 2.0] {
        // The feature's centre projects to x = 50.31 points.
        let camera = key.start() as f64 + 51_215.0 - 50.31 * 1_024.0;
        let lanes = [LaneScene {
            projection: lane([0.0, 2.0, 120.0, 20.0], camera, 1.0 / 1_024.0),
            scene: &scene,
            track: 1,
            selected: None,
            hovered: None,
        }];
        let (image, oracle) = render_lanes(&gpu, &mut renderer, &lanes, [120, 24], scale);
        oracle.assert_matches(&image, &format!("one-point minimum at {scale}x"));
        let row = ((2.0 + 3.0 + 5.0) * scale) as u32;
        let columns = painted_columns(&image, row).unwrap();
        assert_eq!(f64::from(columns.1 - columns.0), scale, "{columns:?}");
    }
}

#[test]
fn pages_are_charged_exactly_evicted_by_lru_and_refused_whole_never_truncated() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let scene_with = |index: u64, features: u64| {
        let key = TrackTileKey { level: 0, index };
        feature_scene(
            key,
            (0..features)
                .map(|feature_index| {
                    feature(
                        feature_index,
                        key.start() + feature_index * 2,
                        key.start() + feature_index * 2 + 1,
                        Strand::Unstranded,
                        0,
                    )
                })
                .collect(),
        )
    };
    let scenes = (0..4)
        .map(|index| scene_with(index, 400))
        .collect::<Vec<_>>();
    let page_bytes = 400 * TRACK_TILE_INSTANCE_BYTES;
    assert_eq!(scenes[0].instance_bytes(), page_bytes);
    // Room for the page table and two and a half pages.
    let ledger = AllocationLedger::new(page_bytes * 5 / 2 + 2_048, 0);
    let mut renderer = TrackTileRenderer::new(&gpu.device, FORMAT, ledger.clone());
    let keys = scenes
        .iter()
        .map(|scene| page_key(9, scene))
        .collect::<Vec<_>>();
    for index in 0..2 {
        let admission = renderer
            .admit_page(&gpu.device, keys[index], &scenes[index], &[])
            .unwrap();
        assert_eq!(
            (admission.already_resident, admission.bytes),
            (false, page_bytes)
        );
    }
    // The shared ledger holds exactly the instance bytes plus the page table.
    let charged = ledger.progress().unwrap().charged;
    assert_eq!(charged, renderer.resident_page_bytes().unwrap());
    assert_eq!(charged - renderer.page_table_bytes(), 2 * page_bytes);
    // Re-admitting is free.
    assert!(
        renderer
            .admit_page(&gpu.device, keys[0], &scenes[0], &[])
            .unwrap()
            .already_resident
    );
    assert_eq!(ledger.progress().unwrap().charged, charged);

    // A third page does not fit: the least recently used unprotected page (1) goes.
    renderer
        .admit_page(&gpu.device, keys[2], &scenes[2], &[keys[2]])
        .unwrap();
    assert!(renderer.is_resident(keys[0]));
    assert!(!renderer.is_resident(keys[1]));
    assert!(renderer.is_resident(keys[2]));
    assert_eq!(
        ledger.progress().unwrap().charged - renderer.page_table_bytes(),
        2 * page_bytes
    );

    // With both residents protected (the frame needs them) a further page is refused whole.
    let error = renderer
        .admit_page(
            &gpu.device,
            keys[3],
            &scenes[3],
            &[keys[0], keys[2], keys[3]],
        )
        .unwrap_err();
    assert!(
        matches!(error, TrackTileRenderError::Budget { required, .. } if required == page_bytes),
        "{error}"
    );
    assert_eq!(renderer.page_count(), 2);
    // A page larger than the whole budget is refused too, not cut down to what fits.
    let huge = scene_with(7, 4_000);
    assert!(matches!(
        renderer.admit_page(&gpu.device, page_key(9, &huge), &huge, &[]),
        Err(TrackTileRenderError::Budget { .. })
    ));
    // The frame reports the page it could not draw.
    let projection = lane([0.0, 0.0, 100.0, 20.0], 0.0, 1.0);
    let report = renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &TrackTileFrame {
            screen_size: [100.0, 20.0],
            scale_factor: 1.0,
            target_size_pixels: [100, 20],
            lanes: vec![TrackLaneDraw {
                projection,
                pages: [keys[0], keys[3]]
                    .into_iter()
                    .map(|key| TrackLanePage {
                        key,
                        selected: None,
                        hovered: None,
                    })
                    .collect(),
            }],
        },
    );
    assert_eq!(report.missing_pages, [keys[3]]);
    assert_eq!((report.draws, report.instances), (1, 400));

    // Retiring the track releases every byte.
    renderer.retire_track(9);
    assert_eq!(renderer.page_count(), 0);
    assert_eq!(
        ledger.progress().unwrap().charged,
        renderer.page_table_bytes()
    );
    renderer.clear();
    assert_eq!(ledger.progress().unwrap().charged, 0);
}

#[test]
fn many_lanes_draw_in_one_pass_without_any_per_track_limit() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    // 200 lanes (well past the alignment cap of 64), two pixels each.
    let key = TrackTileKey { level: 0, index: 0 };
    let scenes = (0..200_u64)
        .map(|track| {
            feature_scene(
                key,
                vec![feature(
                    track,
                    10 + track,
                    60 + track,
                    Strand::Unstranded,
                    0,
                )],
            )
        })
        .collect::<Vec<_>>();
    let mut renderer = TrackTileRenderer::new(
        &gpu.device,
        FORMAT,
        AllocationLedger::new(64 << 20, 1 << 20),
    );
    let lanes = scenes
        .iter()
        .enumerate()
        .map(|(track, scene)| {
            let mut projection = lane([0.0, track as f32 * 2.0, 300.0, 2.0], 0.37, 1.0);
            projection.row_pitch = 2.0;
            projection.row_height = 2.0;
            projection.row_offset = 0.0;
            LaneScene {
                projection,
                scene,
                track: track as u64,
                selected: None,
                hovered: None,
            }
        })
        .collect::<Vec<_>>();
    let (image, oracle) = render_lanes(&gpu, &mut renderer, &lanes, [300, 400], 1.0);
    let comparison = oracle.assert_matches(&image, "200 lanes");
    assert!(comparison.painted >= 200 * 2 * 49, "{comparison:?}");
    assert_eq!(renderer.page_count(), 200);
}
