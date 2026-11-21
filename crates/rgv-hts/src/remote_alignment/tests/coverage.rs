//! Coverage-pyramid tiles built at decode time: every backend produces, for the same filter
//! settings, exactly the depth the existing exact query path reports per base, aggregated to
//! bins.

use std::{
    fs,
    num::NonZeroUsize,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use rgv_core::ContigId;

use super::{
    BamFixture,
    differential::{
        CHR1_LENGTH, interval, open_local_noodles, option_variants, write_bam_fixture,
        write_cram_fixture,
    },
    write_dense_bam,
};
use crate::{
    AlignmentError, AlignmentOpenOptions, AlignmentSource, CoverageBin, CoverageEligibility,
    CoveragePyramidParameters, CoverageTile, CoverageTileId, CoverageTileOutcome,
    CoverageTileProduction, CoverageTileRequest, LocalAlignmentSource, ProductionAlignmentSource,
    QueryDetail, QueryOptions, RemoteCancellation, RemoteError, RemoteTransferBudget,
    derive_coarser_tiles,
};

fn never_cancelled() -> RemoteCancellation {
    Arc::new(|| true)
}

/// Exact per-base depth from the existing one-shot query path: one pileup bin per base.
fn per_base_depth(
    oracle: &mut LocalAlignmentSource,
    contig: u32,
    length: u64,
    options: &QueryOptions,
) -> Vec<u32> {
    let query = oracle
        .query(
            interval(contig, 0, length),
            QueryOptions {
                detail: QueryDetail::Layout,
                collect_alleles: false,
                collect_modifications: false,
                coverage_bins: NonZeroUsize::new(usize::try_from(length).unwrap()).unwrap(),
                ..options.clone()
            },
            &|| true,
        )
        .unwrap();
    assert!(query.pileup.provenance.exact_depth_complete);
    assert!(query.pileup.provenance.overlap_collapse_complete);
    query.pileup.bins.iter().map(|bin| bin.depth).collect()
}

fn expected_tiles(
    depth: &[u32],
    parameters: CoveragePyramidParameters,
    contig: u32,
    level: u8,
) -> Vec<CoverageTile> {
    let contig_length = depth.len() as u64;
    let bin_bases = usize::try_from(parameters.bin_bases(level).unwrap()).unwrap();
    let bins = depth
        .chunks(bin_bases)
        .map(|bases| CoverageBin {
            depth_sum: bases.iter().map(|depth| u64::from(*depth)).sum(),
            minimum_depth: *bases.iter().min().unwrap(),
            maximum_depth: *bases.iter().max().unwrap(),
        })
        .collect::<Vec<_>>();
    bins.chunks(parameters.tile_bins() as usize)
        .enumerate()
        .map(|(index, bins)| {
            CoverageTile::from_bins(
                CoverageTileId::new(ContigId::new(contig), level, index as u64),
                parameters,
                contig_length,
                bins.to_vec(),
            )
            .unwrap()
        })
        .collect()
}

fn complete(outcome: CoverageTileOutcome) -> CoverageTileProduction {
    match outcome {
        CoverageTileOutcome::Complete(production) => production,
        CoverageTileOutcome::Cancelled => panic!("coverage request was cancelled"),
    }
}

fn request(
    parameters: CoveragePyramidParameters,
    contig: u32,
    level: u8,
    tiles: std::ops::Range<u64>,
    options: &QueryOptions,
) -> CoverageTileRequest {
    CoverageTileRequest {
        parameters,
        contig: ContigId::new(contig),
        level,
        tiles,
        eligibility: CoverageEligibility::from_query_options(options),
    }
}

/// Small tiles, so the 100 kbp fixture contig has many tiles at several levels.
fn small_parameters() -> CoveragePyramidParameters {
    CoveragePyramidParameters::new(16, 4, 64).unwrap()
}

fn assert_backend_matches_exact_depth(
    oracle: &mut LocalAlignmentSource,
    source: &mut ProductionAlignmentSource,
    backend: &str,
) {
    let cancellation = never_cancelled();
    let mut distinct_depths = std::collections::BTreeSet::new();
    for (name, options) in option_variants() {
        let depth = per_base_depth(oracle, 0, CHR1_LENGTH, &options);
        distinct_depths.insert(depth.clone());
        for parameters in [CoveragePyramidParameters::DEFAULT, small_parameters()] {
            for level in [0, 1, 3] {
                let context = format!("{backend}, {name}, level {level}, {parameters:?}");
                let expected = expected_tiles(&depth, parameters, 0, level);
                let tiles = parameters.tile_range(level, 0, CHR1_LENGTH, CHR1_LENGTH);
                let production = complete(
                    source
                        .coverage_tiles(
                            &request(parameters, 0, level, tiles.clone(), &options),
                            None,
                            &cancellation,
                        )
                        .unwrap(),
                );
                assert!(production.overlap_collapse_complete, "{context}");
                assert!(production.records_seen >= production.records_eligible);
                assert_eq!(production.tiles, expected, "{context}");

                // A sub-range of tiles sees only the records the index returns for it and
                // still equals the same tiles of the whole contig.
                if tiles.end >= 4 {
                    let part = complete(
                        source
                            .coverage_tiles(
                                &request(parameters, 0, level, 1..3, &options),
                                None,
                                &cancellation,
                            )
                            .unwrap(),
                    );
                    assert_eq!(part.tiles, expected[1..3], "{context}, tiles 1..3");
                }
            }
        }
        // The short second contig: one tile whose last bin is partial.
        let depth = per_base_depth(oracle, 1, 500, &options);
        let production = complete(
            source
                .coverage_tiles(
                    &request(CoveragePyramidParameters::DEFAULT, 1, 0, 0..1, &options),
                    None,
                    &cancellation,
                )
                .unwrap(),
        );
        assert_eq!(
            production.tiles,
            expected_tiles(&depth, CoveragePyramidParameters::DEFAULT, 1, 0),
            "{backend}, {name}, chr2"
        );
        assert_eq!(production.tiles[0].bins().len(), 32);
    }
    // The settings really select different record populations (secondary and supplementary
    // visibility, duplicates and mapping quality, count-once against count-both, tag filters).
    assert!(distinct_depths.len() >= 5, "{}", distinct_depths.len());
}

#[test]
fn every_bam_backend_builds_the_exact_depth_of_the_query_path_for_each_filter_setting() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = write_bam_fixture(directory.path());
    let mut oracle = LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();

    let mut htslib =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    assert!(matches!(htslib, ProductionAlignmentSource::Htslib(_)));
    assert_backend_matches_exact_depth(&mut oracle, &mut htslib, "HTSlib");

    for threads in [0, 4] {
        let mut noodles = open_local_noodles(&path, threads);
        assert!(matches!(noodles, ProductionAlignmentSource::LocalBam(_)));
        assert_backend_matches_exact_depth(&mut oracle, &mut noodles, "noodles local");
    }

    let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), fs::read(&index).unwrap())
    else {
        return;
    };
    let mut remote = ProductionAlignmentSource::open(
        Path::new(&fixture.bam_url),
        &AlignmentOpenOptions::default(),
    )
    .unwrap();
    assert!(matches!(remote, ProductionAlignmentSource::RemoteBam(_)));
    assert_backend_matches_exact_depth(&mut oracle, &mut remote, "noodles remote");
}

#[test]
fn both_cram_decoders_build_the_exact_depth_of_the_query_path() {
    let directory = tempfile::tempdir().unwrap();
    let (path, reference) = write_cram_fixture(directory.path());
    let options = AlignmentOpenOptions {
        reference_path: Some(reference),
        ..AlignmentOpenOptions::default()
    };
    let mut oracle = LocalAlignmentSource::open(&path, &options).unwrap();
    let mut noodles = ProductionAlignmentSource::open(&path, &options).unwrap();
    assert!(matches!(noodles, ProductionAlignmentSource::LocalCram(_)));
    assert_backend_matches_exact_depth(&mut oracle, &mut noodles, "noodles CRAM");
    let mut htslib =
        ProductionAlignmentSource::Htslib(LocalAlignmentSource::open(&path, &options).unwrap());
    assert_backend_matches_exact_depth(&mut oracle, &mut htslib, "HTSlib CRAM");
}

#[test]
fn a_whole_contig_root_tile_equals_the_reduction_of_finer_tiles() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dense.bam");
    write_dense_bam(&path, 40_000);
    let mut source =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    let parameters = CoveragePyramidParameters::DEFAULT;
    let contig_length = 16_000_000;
    let options = QueryOptions::default();
    let cancellation = never_cancelled();
    let root = parameters.root_level(contig_length);
    assert_eq!(root, 5);
    let whole = complete(
        source
            .coverage_tiles(
                &request(parameters, 0, root, 0..1, &options),
                None,
                &cancellation,
            )
            .unwrap(),
    );
    assert_eq!(whole.records_eligible, 40_000);
    let (total, bases) = whole.tiles[0].aggregate(0, contig_length).unwrap();
    assert_eq!(bases, contig_length);
    assert_eq!(total.depth_sum, 40_000 * 150);
    assert_eq!((total.minimum_depth, total.maximum_depth), (0, 2));
    // Bins and boundaries only: a 16 Mbp contig in a few kilobytes of working state.
    assert!(whole.telemetry.peak_depth_state_bytes() < 64 * 1024);
    assert!(whole.telemetry.peak_pending_boundaries <= 4);

    // Produced as bounded 4 Mbp units two levels finer, then reduced: identical.
    let unit_level = root - 2;
    let units = parameters.tile_range(unit_level, 0, contig_length, contig_length);
    let mut tiles = Vec::new();
    for unit in units {
        tiles.extend(
            complete(
                source
                    .coverage_tiles(
                        &request(parameters, 0, unit_level, unit..unit + 1, &options),
                        None,
                        &cancellation,
                    )
                    .unwrap(),
            )
            .tiles,
        );
    }
    let derived = derive_coarser_tiles(&tiles);
    assert_eq!(derived.last(), whole.tiles.first());
}

#[test]
fn a_remote_request_honours_its_transfer_budget_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dense.bam");
    write_dense_bam(&path, 40_000);
    let index = format!("{}.bai", path.display());
    let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), fs::read(&index).unwrap())
    else {
        return;
    };
    let mut source = ProductionAlignmentSource::open(
        Path::new(&fixture.bam_url),
        &AlignmentOpenOptions::default(),
    )
    .unwrap();
    let parameters = CoveragePyramidParameters::DEFAULT;
    let options = QueryOptions::default();
    let whole = request(parameters, 0, 5, 0..1, &options);

    // The whole contig needs far more than 4 KiB of BAM.
    let refused = source.coverage_tiles(
        &whole,
        Some(RemoteTransferBudget {
            maximum_requests: 64,
            maximum_response_bytes: 4 * 1024,
        }),
        &never_cancelled(),
    );
    assert!(
        matches!(
            refused,
            Err(AlignmentError::Remote(RemoteError::TransferBudgetExceeded))
        ) || matches!(&refused, Err(AlignmentError::Io(error))
            if error.to_string().contains("budget")),
        "{refused:?}"
    );

    // The budget does not outlive the call: the same request then completes, and equals the
    // local file's tiles.
    let remote = complete(
        source
            .coverage_tiles(&whole, None, &never_cancelled())
            .unwrap(),
    );
    let mut local =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    let expected = complete(
        local
            .coverage_tiles(&whole, None, &never_cancelled())
            .unwrap(),
    );
    assert_eq!(remote.tiles, expected.tiles);

    // Cancelled before any work, and part-way through the records.
    let stopped: RemoteCancellation = Arc::new(|| false);
    assert_eq!(
        source.coverage_tiles(&whole, None, &stopped).unwrap(),
        CoverageTileOutcome::Cancelled
    );
    for source in [&mut source, &mut local] {
        let polls = Arc::new(AtomicU64::new(0));
        let probe = Arc::clone(&polls);
        let later: RemoteCancellation =
            Arc::new(move || probe.fetch_add(1, Ordering::Relaxed) < 20);
        assert_eq!(
            source.coverage_tiles(&whole, None, &later).unwrap(),
            CoverageTileOutcome::Cancelled
        );
        assert!(polls.load(Ordering::Relaxed) >= 20);
    }
}
