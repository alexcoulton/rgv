use rgv_core::OverlappingMatePolicy;
use rgv_model::{
    AlignmentFlags, AlignmentMateAlignment, CigarOp, CigarOpKind, CoverageAccumulator,
};

use super::*;
use crate::AlignmentMemoryPolicy;

const CONTIG: ContigId = ContigId::new(2);
const CONTIG_LENGTH: u64 = 10_000_000;

/// 1 KiB tiles: level 6 tiles span 4,194,304 bases (one production unit), the root is level 7.
fn small() -> CoveragePyramidParameters {
    CoveragePyramidParameters::new(16, 4, 64).unwrap()
}

fn ledger(budget: usize) -> AllocationLedger {
    AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, budget, budget, budget)
}

/// Depth 2 everywhere, plus a 3-base 900x spike at 6,000,000 and a hole at 1,000,000..1,000,050.
fn reads() -> Vec<(u64, u32)> {
    let mut reads = vec![
        (0, 1_000_000),
        (0, 1_000_000),
        (1_000_050, 8_999_950),
        (1_000_050, 8_999_950),
    ];
    reads.extend(std::iter::repeat_n((6_000_000, 3), 898));
    reads.sort_unstable();
    reads
}

fn accumulate(
    parameters: CoveragePyramidParameters,
    level: u8,
    tiles: std::ops::Range<u64>,
) -> Vec<CoverageTile> {
    let mut accumulator = CoverageAccumulator::new(
        parameters,
        CONTIG,
        CONTIG_LENGTH,
        level,
        tiles,
        OverlappingMatePolicy::CountOnce,
    )
    .unwrap();
    let (span_start, span_end) = accumulator.span();
    for (index, (start, length)) in reads().into_iter().enumerate() {
        if start >= span_end || start + u64::from(length) <= span_start {
            continue;
        }
        let name = format!("read{index}");
        accumulator
            .add_record(
                AlignmentMateAlignment {
                    read_name: name.as_bytes(),
                    contig: CONTIG,
                    start,
                    end: start + u64::from(length),
                    flags: AlignmentFlags::from_bits(0),
                    mate_contig: None,
                    mate_start: None,
                },
                &[CigarOp {
                    kind: CigarOpKind::Match,
                    len: length,
                }],
            )
            .unwrap();
    }
    accumulator.finish().unwrap().tiles
}

fn view(start: u64, end: u64, bases_per_pixel: f64) -> DepthPyramidView {
    DepthPyramidView {
        contig: CONTIG,
        contig_length: CONTIG_LENGTH,
        start,
        end,
        bases_per_pixel,
    }
}

fn summary_charged(ledger: &AllocationLedger) -> usize {
    ledger
        .owner_usage(AllocationOwner::Summary)
        .unwrap()
        .charged
}

#[test]
fn resident_tiles_are_charged_exactly_to_the_summary_owner() {
    let ledger = ledger(1 << 30);
    let mut store = DepthPyramidStore::new(small(), ledger.clone(), 64 << 20);
    assert_eq!(summary_charged(&ledger), 0);
    let tiles = accumulate(small(), 6, 0..3);
    let mut expected = 0;
    for tile in tiles {
        let heap = tile.owned_heap_bytes();
        let resident = store.insert(tile).unwrap();
        expected += size_of::<DepthPyramidTile>() + heap;
        assert_eq!(
            resident.allocation().snapshot().unwrap().charged_capacity,
            resident.estimated_bytes()
        );
    }
    assert_eq!(store.len(), 3);
    assert_eq!(store.resident_bytes(), expected);
    assert_eq!(summary_charged(&ledger), expected);
    // Nothing is charged to any other owner.
    for owner in [
        AllocationOwner::Evidence,
        AllocationOwner::Presentation,
        AllocationOwner::Gpu,
    ] {
        assert_eq!(ledger.owner_usage(owner).unwrap().charged, 0);
    }
    // Replacing a tile does not double-charge.
    let again = accumulate(small(), 6, 1..2).remove(0);
    store.insert(again).unwrap();
    assert_eq!(store.resident_bytes(), expected);
    assert_eq!(summary_charged(&ledger), expected);

    // A published lane keeps its tiles charged after the store lets go of them; the charge
    // returns when the lane is dropped.
    let lane = store.lane(&view(0, CONTIG_LENGTH, 100_000.0));
    store.clear();
    assert_eq!(store.resident_bytes(), 0);
    assert_eq!(summary_charged(&ledger), expected);
    drop(lane);
    assert_eq!(summary_charged(&ledger), 0);
}

#[test]
fn the_store_stays_within_its_cap_and_evicts_least_recently_used_tiles() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    let contig_length = 1 << 34;
    let ledger = ledger(1 << 30);
    // The configured limit is below the floor, so the floor (4 MiB) applies; the Summary
    // share (100 MiB) / 8 is above it.
    let mut store = DepthPyramidStore::new(parameters, ledger.clone(), 1);
    assert_eq!(
        store.effective_byte_limit(),
        DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES
    );
    let tile = |index: u64| {
        CoverageTile::from_bins(
            CoverageTileId::new(CONTIG, 0, index),
            parameters,
            contig_length,
            vec![
                CoverageBin {
                    depth_sum: 16 * 30,
                    minimum_depth: 30,
                    maximum_depth: 30,
                };
                1_024
            ],
        )
        .unwrap()
    };
    let per_tile = size_of::<DepthPyramidTile>() + tile(0).owned_heap_bytes();
    let capacity = DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES / per_tile;
    for index in 0..capacity as u64 {
        store.insert(tile(index)).unwrap();
    }
    assert_eq!(store.len(), capacity);
    assert_eq!(store.evictions(), 0);
    // Tile 0 is the oldest insertion but the most recently used.
    assert!(store.get(CoverageTileId::new(CONTIG, 0, 0)).is_some());
    for index in capacity as u64..capacity as u64 + 10 {
        store.insert(tile(index)).unwrap();
        assert!(store.resident_bytes() <= store.effective_byte_limit());
        assert_eq!(summary_charged(&ledger), store.resident_bytes());
    }
    assert_eq!(store.evictions(), 10);
    assert!(store.contains(CoverageTileId::new(CONTIG, 0, 0)));
    for index in 1..=10 {
        assert!(!store.contains(CoverageTileId::new(CONTIG, 0, index)));
    }
    assert!(store.contains(CoverageTileId::new(CONTIG, 0, 11)));

    // A small cache budget shrinks the pyramid's share with it, down to the floor only.
    let tight = AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, 64 << 20, 0, 0);
    let limited = DepthPyramidStore::new(parameters, tight, 1 << 30);
    assert_eq!(
        limited.effective_byte_limit(),
        DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES
    );
    let roomy = DepthPyramidStore::new(parameters, self::ledger(8 << 30), 1 << 30);
    assert_eq!(
        roomy.effective_byte_limit(),
        (8_usize << 30) / 10 / DEPTH_PYRAMID_SUMMARY_SHARE_DIVISOR
    );

    // A tile of another geometry is refused and charges nothing.
    let before = summary_charged(&ledger);
    let foreign = CoverageTile::from_bins(
        CoverageTileId::new(CONTIG, 0, 0),
        small(),
        1_024,
        vec![CoverageBin::default(); 64],
    )
    .unwrap();
    assert!(store.insert(foreign).is_err());
    assert_eq!(summary_charged(&ledger), before);
}

#[test]
fn a_plan_selects_the_level_for_the_camera_and_lists_visible_units_before_the_margin() {
    let store = DepthPyramidStore::new(small(), ledger(1 << 30), 64 << 20);
    // Whole contig on 100 pixels: level 7 bins (262,144 bp) are the first at least a pixel
    // wide; the root level is 7 as well. Units are level-6 tiles (4.19 Mbp each).
    let whole = view(0, CONTIG_LENGTH, 100_000.0);
    let plan = store.plan(&whole);
    assert_eq!((plan.target_level, plan.production_level), (7, 6));
    assert_eq!(
        plan.missing_visible,
        (0..3)
            .map(|index| CoverageTileId::new(CONTIG, 6, index))
            .collect::<Vec<_>>()
    );
    assert!(plan.missing_margin.is_empty());
    assert_eq!(plan.next(), Some(CoverageTileId::new(CONTIG, 6, 0)));

    // A narrow view: 20 bp per pixel is level 1 (64 bp bins), produced directly.
    let narrow = view(5_000_000, 5_020_000, 20.0);
    let plan = store.plan(&narrow);
    assert_eq!((plan.target_level, plan.production_level), (1, 1));
    let tile_bases = small().tile_bases(1).unwrap();
    assert_eq!(tile_bases, 4_096);
    let visible = (5_000_000 / tile_bases..=5_019_999 / tile_bases).collect::<Vec<_>>();
    assert_eq!(
        plan.missing_visible
            .iter()
            .map(|id| id.index)
            .collect::<Vec<_>>(),
        visible
    );
    // The margin is half a view on either side, nearest tiles first, alternating sides.
    assert!(!plan.missing_margin.is_empty());
    let (margin_start, margin_end) = narrow.with_margin();
    assert_eq!((margin_start, margin_end), (4_990_000, 5_030_000));
    let first = *visible.first().unwrap();
    let last = *visible.last().unwrap();
    assert_eq!(
        plan.missing_margin
            .iter()
            .take(2)
            .map(|id| id.index)
            .collect::<std::collections::BTreeSet<_>>(),
        [first - 1, last + 1].into_iter().collect()
    );
    assert!(
        plan.missing_margin
            .iter()
            .all(|id| !visible.contains(&id.index))
    );

    // Bins never get wider than the root needs, however far out the camera is.
    assert_eq!(store.target_level(&view(0, CONTIG_LENGTH, 1.0e9)), 7);
    // Below the base bin width the finest level serves.
    assert_eq!(store.target_level(&view(0, 1_000, 0.5)), 0);
}

#[test]
fn a_lane_shows_coarse_coverage_first_and_refines_as_units_arrive() {
    let ledger = ledger(1 << 30);
    let mut store = DepthPyramidStore::new(small(), ledger, 64 << 20);
    let whole = view(0, CONTIG_LENGTH, 100_000.0);

    // Nothing resident: an empty lane, and the plan wants every unit.
    let lane = store.lane(&whole);
    assert!(lane.segments().is_empty());
    assert!(!lane.covers_visible() && !lane.is_exact_at_target());
    assert_eq!(lane.columns(0.0, 1.0e7, 100)[50].bases, 0);

    // Units arrive one at a time; the lane shows each as soon as it is resident.
    let units = accumulate(small(), 6, 0..3);
    store.insert(units[0].clone()).unwrap();
    let lane = store.lane(&whole);
    assert_eq!(lane.segments().len(), 1);
    assert_eq!(
        (lane.segments()[0].start, lane.segments()[0].end),
        (0, 4_194_304)
    );
    assert!(!lane.covers_visible());
    assert_eq!(store.plan(&whole).missing_visible.len(), 2);

    store.insert(units[1].clone()).unwrap();
    store.insert(units[2].clone()).unwrap();
    let lane = store.lane(&whole);
    assert!(lane.covers_visible());
    // Finer than the target: complete, but still to be reduced to the target level.
    assert!(!lane.is_exact_at_target());
    assert!(store.plan(&whole).is_complete());
    let finer_signature = lane.signature();
    assert_eq!(finer_signature.len(), 3);

    // The root is derived from the three units without decoding or loading anything.
    let mut created = Vec::new();
    let root = store
        .obtain(
            CoverageTileId::new(CONTIG, 7, 0),
            CONTIG_LENGTH,
            DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH,
            &mut |_| None,
            &mut created,
        )
        .unwrap()
        .unwrap();
    assert_eq!(created.len(), 1);
    assert!(created[0].1, "a derived tile is flagged for persisting");
    assert_eq!(root.tile(), &accumulate(small(), 7, 0..1)[0]);
    let lane = store.lane(&whole);
    assert!(lane.is_exact_at_target() && lane.covers_visible());
    assert_eq!(lane.signature(), [(root.tile().id(), 0, CONTIG_LENGTH)]);
    assert_ne!(lane.signature(), finer_signature);

    // Zooming in: the root is shown at once (coarse first) while level-4 units are missing...
    let closer = view(5_900_000, 6_100_000, 2_000.0);
    assert_eq!(store.target_level(&closer), 4);
    let coarse = store.lane(&closer);
    assert!(coarse.covers_visible() && !coarse.is_exact_at_target());
    assert!(
        coarse
            .segments()
            .iter()
            .all(|segment| segment.tile.tile().id().level > 4)
    );
    let plan = store.plan(&closer);
    assert_eq!((plan.target_level, plan.production_level), (4, 4));
    assert!(!plan.missing_visible.is_empty());
    // ...and each arriving unit replaces its part of the coarse picture.
    let mut previous = coarse.signature();
    for id in plan.missing_visible.iter().chain(&plan.missing_margin) {
        let tile = accumulate(small(), 4, id.index..id.index + 1).remove(0);
        store.insert(tile).unwrap();
        let lane = store.lane(&closer);
        assert!(lane.covers_visible());
        assert_ne!(lane.signature(), previous);
        previous = lane.signature();
        // Segments never overlap and are ordered.
        for pair in lane.segments().windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
    }
    let refined = store.lane(&closer);
    assert!(refined.is_exact_at_target());
    assert!(
        refined
            .segments()
            .iter()
            .all(|segment| segment.tile.tile().id().level == 4)
    );
    assert!(store.plan(&closer).is_complete());
}

#[test]
fn columns_keep_a_narrow_peak_and_a_dropout_at_every_zoom() {
    let mut store = DepthPyramidStore::new(small(), ledger(1 << 30), 64 << 20);
    store
        .insert(accumulate(small(), 7, 0..1).remove(0))
        .unwrap();
    let whole = view(0, CONTIG_LENGTH, 100_000.0);
    let lane = store.lane(&whole);
    #[allow(clippy::cast_precision_loss)]
    let columns = lane.columns(0.0, CONTIG_LENGTH as f64, 100);
    assert_eq!(columns.len(), 100);
    // The 3-base, 900x spike is 0.00003 of a column wide and still sets its column's maximum.
    let spike = columns[60];
    assert_eq!(spike.maximum_depth, 900);
    assert!(spike.mean_depth < 2.1, "{}", spike.mean_depth);
    assert_eq!(spike.minimum_depth, 2);
    // The 50-base hole still sets its column's minimum.
    assert_eq!(columns[10].minimum_depth, 0);
    assert_eq!(columns[10].maximum_depth, 2);
    // Elsewhere the lane is flat.
    for column in [0, 5, 30, 80, 95] {
        assert_eq!(
            (columns[column].minimum_depth, columns[column].maximum_depth),
            (2, 2)
        );
        assert!((columns[column].mean_depth - 2.0).abs() < 1e-9);
        assert!(columns[column].bases > 0);
    }
    assert_eq!(lane.maximum_depth(0, CONTIG_LENGTH), 900);
    assert_eq!(lane.maximum_depth(0, 5_000_000), 2);
}

#[test]
fn tiles_come_from_the_loader_before_anything_is_derived() {
    let mut store = DepthPyramidStore::new(small(), ledger(1 << 30), 64 << 20);
    let root = accumulate(small(), 7, 0..1).remove(0);
    let units = accumulate(small(), 6, 0..3);

    // The root itself is in the sidecar: one load, no children consulted.
    let mut asked = Vec::new();
    let mut created = Vec::new();
    let loaded = store
        .obtain(
            root.id(),
            CONTIG_LENGTH,
            3,
            &mut |id| {
                asked.push(id);
                (id == root.id()).then(|| root.clone())
            },
            &mut created,
        )
        .unwrap()
        .unwrap();
    assert_eq!(asked, [root.id()]);
    assert_eq!(loaded.tile(), &root);
    assert_eq!(created.len(), 1);
    assert!(!created[0].1, "a loaded tile is already persisted");

    // Only the units are in the sidecar: they are loaded and the root derived from them.
    let mut store = DepthPyramidStore::new(small(), ledger(1 << 30), 64 << 20);
    let mut created = Vec::new();
    let derived = store
        .obtain(
            root.id(),
            CONTIG_LENGTH,
            3,
            &mut |id| units.iter().find(|unit| unit.id() == id).cloned(),
            &mut created,
        )
        .unwrap()
        .unwrap();
    assert_eq!(derived.tile(), &root);
    assert_eq!(
        created
            .iter()
            .map(|(_, derived)| *derived)
            .collect::<Vec<_>>(),
        [false, false, false, true]
    );

    // One unit missing everywhere: nothing can be derived, and nothing is guessed.
    let mut store = DepthPyramidStore::new(small(), ledger(1 << 30), 64 << 20);
    let mut created = Vec::new();
    assert!(
        store
            .obtain(
                root.id(),
                CONTIG_LENGTH,
                3,
                &mut |id| units[..2].iter().find(|unit| unit.id() == id).cloned(),
                &mut created,
            )
            .unwrap()
            .is_none()
    );
    // With no derivation depth only the tile itself is tried.
    assert!(
        store
            .obtain(root.id(), CONTIG_LENGTH, 0, &mut |_| None, &mut created)
            .unwrap()
            .is_none()
    );
}

#[test]
fn read_evidence_is_the_depth_authority_whenever_it_is_ready() {
    assert_eq!(
        DepthLaneAuthority::select(true),
        DepthLaneAuthority::ExactReadEvidence
    );
    assert_eq!(
        DepthLaneAuthority::select(false),
        DepthLaneAuthority::Pyramid
    );
}
