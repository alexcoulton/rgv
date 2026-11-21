use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy};

use super::*;
use crate::{AlignmentFlags, ExactDepthTileBuilder};

const CONTIG: ContigId = ContigId::new(3);

/// Deterministic generator (`SplitMix64`); the tests need reproducible, not secure, randomness.
pub(crate) struct Random(u64);

impl Random {
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TestRead {
    pub(crate) name: Vec<u8>,
    pub(crate) start: u64,
    pub(crate) cigar: Vec<CigarOp>,
    pub(crate) flags: u16,
    pub(crate) mate_start: Option<u64>,
}

impl TestRead {
    pub(crate) fn end(&self) -> u64 {
        self.start
            + self
                .cigar
                .iter()
                .filter(|operation| {
                    matches!(
                        operation.kind,
                        CigarOpKind::Match
                            | CigarOpKind::SequenceMatch
                            | CigarOpKind::SequenceMismatch
                            | CigarOpKind::Deletion
                            | CigarOpKind::ReferenceSkip
                    )
                })
                .map(|operation| u64::from(operation.len))
                .sum::<u64>()
    }

    pub(crate) fn mate(&self) -> MateAlignment<'_> {
        MateAlignment {
            read_name: &self.name,
            contig: CONTIG,
            start: self.start,
            end: self.end(),
            flags: AlignmentFlags::from_bits(self.flags),
            mate_contig: self.mate_start.map(|_| CONTIG),
            mate_start: self.mate_start,
        }
    }

    /// Reference positions under an aligned (`M`, `=`, `X`) operation.
    fn aligned_positions(&self) -> Vec<u64> {
        let mut positions = Vec::new();
        let mut cursor = self.start;
        for operation in &self.cigar {
            match operation.kind {
                CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                    positions.extend(cursor..cursor + u64::from(operation.len));
                    cursor += u64::from(operation.len);
                }
                CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                    cursor += u64::from(operation.len);
                }
                _ => {}
            }
        }
        positions
    }
}

fn random_cigar(random: &mut Random) -> Vec<CigarOp> {
    let op = |kind, len: u64| CigarOp {
        kind,
        len: u32::try_from(len).unwrap(),
    };
    let mut cigar = Vec::new();
    if random.below(4) == 0 {
        cigar.push(op(CigarOpKind::SoftClip, 1 + random.below(9)));
    }
    cigar.push(op(CigarOpKind::Match, 1 + random.below(120)));
    for _ in 0..random.below(4) {
        match random.below(5) {
            0 => cigar.push(op(CigarOpKind::Deletion, 1 + random.below(30))),
            1 => cigar.push(op(CigarOpKind::ReferenceSkip, 1 + random.below(900))),
            2 => cigar.push(op(CigarOpKind::Insertion, 1 + random.below(6))),
            3 => cigar.push(op(CigarOpKind::SequenceMismatch, 1 + random.below(3))),
            _ => cigar.push(op(CigarOpKind::SequenceMatch, 1 + random.below(40))),
        }
        cigar.push(op(CigarOpKind::Match, 1 + random.below(90)));
    }
    cigar
}

/// Singletons and overlapping, abutting and distant pairs, in coordinate order.
pub(crate) fn random_reads(seed: u64, templates: usize, contig_length: u64) -> Vec<TestRead> {
    let mut random = Random::new(seed);
    let mut reads = Vec::new();
    for template in 0..templates {
        let name = format!("template-{template}").into_bytes();
        let start = random.below(contig_length - 1);
        let first = TestRead {
            name: name.clone(),
            start,
            cigar: random_cigar(&mut random),
            flags: 0,
            mate_start: None,
        };
        if random.below(3) == 0 {
            reads.push(first);
            continue;
        }
        // Mates start anywhere from the same base to past the first mate's end.
        let mate_start = start + random.below(first.end() - start + 60);
        let second = TestRead {
            name,
            start: mate_start,
            cigar: random_cigar(&mut random),
            flags: 0x1 | 0x80 | 0x10,
            mate_start: Some(start),
        };
        reads.push(TestRead {
            flags: 0x1 | 0x40,
            mate_start: Some(mate_start),
            ..first
        });
        reads.push(second);
    }
    reads.sort_by_key(|read| read.start);
    reads
}

/// Per-base depth computed without any of the production code: a template counts once at every
/// base either mate aligns (or once per mate under `CountBoth`).
fn brute_force_depth(
    reads: &[TestRead],
    contig_length: u64,
    policy: OverlappingMatePolicy,
) -> Vec<u32> {
    let mut depth = vec![0_u32; usize::try_from(contig_length).unwrap()];
    let mut by_template = std::collections::BTreeMap::<&[u8], Vec<&TestRead>>::new();
    for read in reads {
        by_template.entry(&read.name).or_default().push(read);
    }
    for mates in by_template.values() {
        let mut positions = Vec::new();
        for mate in mates {
            positions.extend(mate.aligned_positions());
        }
        if policy == OverlappingMatePolicy::CountOnce {
            positions.sort_unstable();
            positions.dedup();
        }
        for position in positions {
            if let Some(depth) = depth.get_mut(usize::try_from(position).unwrap()) {
                *depth += 1;
            }
        }
    }
    depth
}

fn bins_from_depth(depth: &[u32], bin_bases: u64) -> Vec<CoverageBin> {
    depth
        .chunks(usize::try_from(bin_bases).unwrap())
        .map(|bases| CoverageBin {
            depth_sum: bases.iter().map(|depth| u64::from(*depth)).sum(),
            minimum_depth: *bases.iter().min().unwrap(),
            maximum_depth: *bases.iter().max().unwrap(),
        })
        .collect()
}

pub(crate) fn accumulate(
    reads: &[TestRead],
    parameters: CoveragePyramidParameters,
    contig_length: u64,
    level: u8,
    policy: OverlappingMatePolicy,
) -> CoverageAccumulation {
    let tiles = parameters.tile_range(level, 0, contig_length, contig_length);
    let mut accumulator =
        CoverageAccumulator::new(parameters, CONTIG, contig_length, level, tiles, policy).unwrap();
    for read in reads {
        accumulator.add_record(read.mate(), &read.cigar).unwrap();
    }
    accumulator.finish().unwrap()
}

fn all_bins(tiles: &[CoverageTile]) -> Vec<CoverageBin> {
    tiles
        .iter()
        .flat_map(|tile| tile.bins().iter().copied())
        .collect()
}

#[test]
fn every_level_equals_brute_force_per_base_depth_on_random_reads() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    for (seed, policy) in [
        (1, OverlappingMatePolicy::CountOnce),
        (2, OverlappingMatePolicy::CountOnce),
        (3, OverlappingMatePolicy::CountBoth),
        (4, OverlappingMatePolicy::CountOnce),
    ] {
        // Not a multiple of any bin width, so the last bin of every level is partial.
        let contig_length = 70_003 + seed * 1_117;
        let reads = random_reads(seed, 1_500, contig_length);
        let depth = brute_force_depth(&reads, contig_length, policy);
        assert!(depth.iter().any(|depth| *depth > 12));
        assert!(depth.contains(&0));
        for level in 0..4 {
            let accumulation = accumulate(&reads, parameters, contig_length, level, policy);
            assert!(accumulation.overlap_collapse_complete);
            assert_eq!(accumulation.eligible_records, reads.len() as u64);
            let expected = bins_from_depth(&depth, parameters.bin_bases(level).unwrap());
            assert_eq!(
                all_bins(&accumulation.tiles),
                expected,
                "seed {seed} level {level}"
            );
        }
    }
}

/// The exact depth summary of the read-level path: one `ExactDepthTile` per 4,096-base tile,
/// each with its own collapser over the records that intersect it, exactly as the alignment
/// scene builds them.
fn exact_depth_tiles(
    reads: &[TestRead],
    contig_length: u64,
    policy: OverlappingMatePolicy,
) -> Vec<crate::ExactDepthTile> {
    const TILE: u64 = 4_096;
    (0..contig_length.div_ceil(TILE))
        .map(|index| {
            let start = index * TILE;
            let end = (start + TILE).min(contig_length);
            let mut builder =
                ExactDepthTileBuilder::new(GenomicInterval::new(CONTIG, start, end).unwrap());
            let mut collapser = OverlapCollapser::new(policy);
            let mut scratch = Vec::new();
            for read in reads
                .iter()
                .filter(|read| read.start < end && read.end() > start)
            {
                add_counted_depth_segments(
                    &mut collapser,
                    read.mate(),
                    &read.cigar,
                    start,
                    end,
                    &mut scratch,
                    |start, end| builder.add_aligned_segment(start, end, false, None),
                );
            }
            builder.finish()
        })
        .collect()
}

#[test]
fn finest_level_equals_the_exact_depth_summary_aggregated_to_bins() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    for seed in [11, 12, 13] {
        let contig_length = 50_000 + seed * 977;
        let reads = random_reads(seed, 5_000, contig_length);
        let policy = OverlappingMatePolicy::CountOnce;
        let exact = exact_depth_tiles(&reads, contig_length, policy);
        let accumulation = accumulate(&reads, parameters, contig_length, 0, policy);
        assert!(accumulation.collapsed_mate_bases > 0);
        for tile in &accumulation.tiles {
            let from_exact = CoverageTile::from_depth_runs(
                tile.id(),
                parameters,
                contig_length,
                exact.iter().flat_map(|tile| tile.runs().iter()),
            )
            .unwrap();
            assert_eq!(tile, &from_exact, "seed {seed} tile {:?}", tile.id());
            // The exact summary's own maximum agrees with the pyramid's.
            let (start, end) = tile.span();
            let exact_maximum = exact
                .iter()
                .filter_map(|exact| {
                    exact.maximum_depth(GenomicInterval::new(CONTIG, start, end).unwrap())
                })
                .max()
                .unwrap();
            assert_eq!(tile.maximum_depth(), exact_maximum);
        }
    }
}

#[test]
fn coarser_levels_derived_from_tiles_equal_the_same_levels_accumulated_from_reads() {
    // Small tiles so that several levels have several tiles.
    let parameters = CoveragePyramidParameters::new(16, 4, 64).unwrap();
    let contig_length = 300_011;
    let reads = random_reads(21, 9_000, contig_length);
    let policy = OverlappingMatePolicy::CountOnce;
    let base = accumulate(&reads, parameters, contig_length, 0, policy);
    let derived = derive_coarser_tiles(&base.tiles);
    let root = parameters.root_level(contig_length);
    assert!(root >= 3);
    for level in 1..=root {
        let direct = accumulate(&reads, parameters, contig_length, level, policy);
        let from_tiles = derived
            .iter()
            .filter(|tile| tile.id().level == level)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(from_tiles, direct.tiles, "level {level}");
    }
    assert_eq!(
        derived.last().map(CoverageTile::id),
        Some(CoverageTileId::new(CONTIG, root, 0))
    );
}

#[test]
fn a_parent_is_not_derived_while_a_child_inside_the_contig_is_missing() {
    let parameters = CoveragePyramidParameters::new(16, 4, 64).unwrap();
    let contig_length = 16 * 64 * 4;
    let reads = random_reads(31, 400, contig_length);
    let mut base = accumulate(
        &reads,
        parameters,
        contig_length,
        0,
        OverlappingMatePolicy::CountOnce,
    );
    assert_eq!(base.tiles.len(), 4);
    assert_eq!(derive_coarser_tiles(&base.tiles).len(), 1);
    base.tiles.remove(2);
    assert!(derive_coarser_tiles(&base.tiles).is_empty());
}

#[test]
fn a_narrow_peak_and_a_dropout_survive_to_the_coarsest_level() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    let contig_length = 4_000_000;
    let op = |kind, len| CigarOp { kind, len };
    let mut reads = Vec::new();
    // Uniform 3x background with a 40-base hole and a 5-base 500x spike.
    for copy in 0..3 {
        reads.push(TestRead {
            name: format!("background-left-{copy}").into_bytes(),
            start: 0,
            cigar: vec![op(CigarOpKind::Match, 2_000_000)],
            flags: 0,
            mate_start: None,
        });
        reads.push(TestRead {
            name: format!("background-right-{copy}").into_bytes(),
            start: 2_000_040,
            cigar: vec![op(CigarOpKind::Match, 1_999_960)],
            flags: 0,
            mate_start: None,
        });
    }
    for copy in 0..497 {
        reads.push(TestRead {
            name: format!("spike-{copy}").into_bytes(),
            start: 1_000_000,
            cigar: vec![op(CigarOpKind::Match, 5)],
            flags: 0,
            mate_start: None,
        });
    }
    reads.sort_by_key(|read| read.start);
    let root = parameters.root_level(contig_length);
    let accumulation = accumulate(
        &reads,
        parameters,
        contig_length,
        root,
        OverlappingMatePolicy::CountOnce,
    );
    assert_eq!(accumulation.tiles.len(), 1);
    let tile = &accumulation.tiles[0];
    let (total, bases) = tile.aggregate(0, contig_length).unwrap();
    assert_eq!(bases, contig_length);
    assert_eq!(total.maximum_depth, 500);
    assert_eq!(total.minimum_depth, 0);
    let mean = total.mean_depth(bases);
    assert!((mean - 3.0).abs() < 0.01, "mean {mean}");
    // The bins holding the spike and the hole report them; their means barely move.
    let spike = tile.aggregate(1_000_000, 1_000_005).unwrap().0;
    assert_eq!(spike.maximum_depth, 500);
    assert_eq!(spike.minimum_depth, 3);
    let hole = tile.aggregate(2_000_000, 2_000_040).unwrap().0;
    assert_eq!(hole.minimum_depth, 0);
    assert_eq!(hole.maximum_depth, 3);
}

#[test]
fn working_memory_does_not_grow_with_depth() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    let contig_length = 200_000_u64;
    let telemetry_at = |depth: u64| {
        let tiles = parameters.tile_range(0, 0, contig_length, contig_length);
        let mut accumulator = CoverageAccumulator::new(
            parameters,
            CONTIG,
            contig_length,
            0,
            tiles,
            OverlappingMatePolicy::CountOnce,
        )
        .unwrap();
        // Records are generated one at a time and dropped: nothing outside the accumulator
        // holds them. 150-base reads, `depth` of them starting every 150 bases, each paired
        // with an overlapping mate 100 bases on.
        let cigar = [CigarOp {
            kind: CigarOpKind::Match,
            len: 150,
        }];
        let mut records = 0_u64;
        let mut name = Vec::new();
        for start in (0..contig_length - 400).step_by(50) {
            for copy in 0..depth / 3 {
                for (flags, own_start, mate_start, template_start) in [
                    (0x1 | 0x40, start, start + 100, start),
                    (
                        0x1 | 0x80,
                        start,
                        start.wrapping_sub(100),
                        start.wrapping_sub(100),
                    ),
                ] {
                    if template_start > contig_length {
                        continue;
                    }
                    name.clear();
                    name.extend_from_slice(format!("t{template_start}c{copy}").as_bytes());
                    accumulator
                        .add_record(
                            MateAlignment {
                                read_name: &name,
                                contig: CONTIG,
                                start: own_start,
                                end: own_start + 150,
                                flags: AlignmentFlags::from_bits(flags),
                                mate_contig: Some(CONTIG),
                                mate_start: Some(mate_start),
                            },
                            &cigar,
                        )
                        .unwrap();
                    records += 1;
                }
            }
        }
        let accumulation = accumulator.finish().unwrap();
        assert!(accumulation.overlap_collapse_complete);
        (accumulation, records)
    };
    let (shallow, shallow_records) = telemetry_at(30);
    let (deep, deep_records) = telemetry_at(3_000);
    assert!(deep_records >= 100 * shallow_records);
    assert!(deep.tiles.iter().map(CoverageTile::maximum_depth).max() >= Some(3_000));
    // Bins and pending depth boundaries are identical: a hundred times the depth costs nothing.
    assert_eq!(
        deep.telemetry.peak_depth_state_bytes(),
        shallow.telemetry.peak_depth_state_bytes()
    );
    assert_eq!(
        deep.telemetry.peak_pending_boundaries,
        shallow.telemetry.peak_pending_boundaries
    );
    assert!(deep.telemetry.peak_pending_boundaries <= 16);
    // The count-once window is the only state that follows depth, and it is capped.
    assert!(deep.telemetry.peak_pending_mates <= crate::pileup::MAX_PENDING_PAIR_OVERLAPS);
}

#[test]
fn out_of_order_and_foreign_records_are_refused() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    let mut accumulator = CoverageAccumulator::new(
        parameters,
        CONTIG,
        100_000,
        0,
        0..1,
        OverlappingMatePolicy::CountOnce,
    )
    .unwrap();
    let cigar = [CigarOp {
        kind: CigarOpKind::Match,
        len: 50,
    }];
    let read = |start, contig| MateAlignment {
        read_name: b"r",
        contig,
        start,
        end: start + 50,
        flags: AlignmentFlags::from_bits(0),
        mate_contig: None,
        mate_start: None,
    };
    accumulator.add_record(read(500, CONTIG), &cigar).unwrap();
    assert_eq!(
        accumulator.add_record(read(499, CONTIG), &cigar),
        Err(CoveragePyramidError::RecordsOutOfOrder)
    );
    assert_eq!(
        accumulator.add_record(read(600, ContigId::new(9)), &cigar),
        Err(CoveragePyramidError::WrongContig)
    );
}

#[test]
fn records_reaching_in_from_outside_the_requested_tiles_are_clipped() {
    let parameters = CoveragePyramidParameters::new(16, 4, 64).unwrap();
    let tile_bases = parameters.tile_bases(0).unwrap();
    let contig_length = tile_bases * 5;
    let reads = random_reads(41, 3_000, contig_length);
    let whole = accumulate(
        &reads,
        parameters,
        contig_length,
        0,
        OverlappingMatePolicy::CountOnce,
    );
    let mut accumulator = CoverageAccumulator::new(
        parameters,
        CONTIG,
        contig_length,
        0,
        2..4,
        OverlappingMatePolicy::CountOnce,
    )
    .unwrap();
    let (start, end) = accumulator.span();
    assert_eq!((start, end), (tile_bases * 2, tile_bases * 4));
    // Only what an index query of the span returns.
    for read in reads
        .iter()
        .filter(|read| read.start < end && read.end() > start)
    {
        accumulator.add_record(read.mate(), &read.cigar).unwrap();
    }
    assert_eq!(accumulator.finish().unwrap().tiles, whole.tiles[2..4]);
}

#[test]
fn level_selection_picks_the_finest_level_whose_bins_cover_a_pixel() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    assert_eq!(parameters.level_for_bases_per_pixel(0.5), 0);
    assert_eq!(parameters.level_for_bases_per_pixel(16.0), 0);
    assert_eq!(parameters.level_for_bases_per_pixel(16.1), 1);
    assert_eq!(parameters.level_for_bases_per_pixel(64.0), 1);
    assert_eq!(parameters.level_for_bases_per_pixel(1_000.0), 3);
    assert_eq!(parameters.level_for_bases_per_pixel(125_000.0), 7);
    assert_eq!(parameters.level_for_bases_per_pixel(f64::MAX), 11);
    for level in 0..=parameters.maximum_level() {
        #[allow(clippy::cast_precision_loss)]
        let bin = parameters.bin_bases(level).unwrap() as f64;
        // Bins are between one and four pixels wide.
        assert_eq!(parameters.level_for_bases_per_pixel(bin), level);
        assert_eq!(parameters.level_for_bases_per_pixel(bin / 3.99), level);
        if level > 0 {
            assert_eq!(parameters.level_for_bases_per_pixel(bin / 4.0), level - 1);
        }
    }
    // A 248 Mbp chromosome fits one tile at level 7.
    assert_eq!(parameters.root_level(248_956_422), 7);
    assert_eq!(parameters.tile_range(0, 16_383, 16_385, 1 << 30), 0..2);
    assert_eq!(parameters.tile_range(0, 0, 10, 0), 0..0);
}

#[test]
fn tiles_reject_inconsistent_or_wrongly_sized_bins() {
    let parameters = CoveragePyramidParameters::DEFAULT;
    let id = CoverageTileId::new(CONTIG, 0, 0);
    let bin = CoverageBin {
        depth_sum: 32,
        minimum_depth: 2,
        maximum_depth: 2,
    };
    assert!(CoverageTile::from_bins(id, parameters, 160, vec![bin; 10]).is_ok());
    assert_eq!(
        CoverageTile::from_bins(id, parameters, 160, vec![bin; 9]),
        Err(CoveragePyramidError::BinCountMismatch {
            expected: 10,
            actual: 9
        })
    );
    let mut bins = vec![bin; 10];
    bins[4].depth_sum = 33;
    assert_eq!(
        CoverageTile::from_bins(id, parameters, 160, bins),
        Err(CoveragePyramidError::InconsistentBin)
    );
    assert_eq!(
        CoverageTile::from_bins(CoverageTileId::new(CONTIG, 0, 1), parameters, 160, vec![]),
        Err(CoveragePyramidError::TileOutsideContig)
    );
    assert!(CoveragePyramidParameters::new(16, 4, 1_022).is_err());
}

#[test]
fn eligibility_digest_input_distinguishes_every_setting() {
    let base = CoverageEligibility::default();
    let mut variants = vec![base.clone()];
    variants.push(CoverageEligibility {
        include_secondary: !base.include_secondary,
        ..base.clone()
    });
    variants.push(CoverageEligibility {
        include_supplementary: !base.include_supplementary,
        ..base.clone()
    });
    variants.push(CoverageEligibility {
        overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
        ..base.clone()
    });
    for filter in [
        AlignmentFilter {
            minimum_mapping_quality: Some(0),
            ..AlignmentFilter::default()
        },
        AlignmentFilter {
            minimum_mapping_quality: Some(20),
            ..AlignmentFilter::default()
        },
        AlignmentFilter {
            excluded_flags: 0x400,
            ..AlignmentFilter::default()
        },
        AlignmentFilter {
            excluded_flags: 0x600,
            ..AlignmentFilter::default()
        },
        AlignmentFilter {
            required_flags: 0x2,
            ..AlignmentFilter::default()
        },
    ] {
        variants.push(CoverageEligibility {
            filter,
            ..base.clone()
        });
    }
    let encodings = variants
        .iter()
        .map(CoverageEligibility::canonical_bytes)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(encodings.len(), variants.len());
    assert_eq!(
        CoverageEligibility::from_query_options(&base.to_query_options()),
        base
    );
}
