use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::AtomicUsize,
};

use rgv_alignment_scene::{AlignmentMemoryPolicy, AllocationOwner};
use rgv_core::GenomicInterval;
use rgv_hts::{AlignmentFilter, CoverageBin, QueryOptions};

use super::*;

const WAIT: Duration = Duration::from_secs(120);
const OWNER: SourceOwnerId = SourceOwnerId::new(7);
/// Both contigs of `mixed.bam` are this long.
const MIXED_CONTIG_LENGTH: u64 = 3_000_000;

fn mixed() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam")
}

fn variants() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam")
}

fn ledger() -> AllocationLedger {
    AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, 1 << 30, 1 << 30, 1 << 30)
}

/// 1 KiB tiles and 262 kbp units, so the 3 Mbp contigs take twelve units and two levels of
/// reduction to reach their root tile (level 6).
fn small_config(path: PathBuf, ledger: AllocationLedger) -> AlignmentCoverageEngineConfig {
    AlignmentCoverageEngineConfig {
        parameters: CoveragePyramidParameters::new(16, 4, 64).unwrap(),
        production_unit_bases: 262_144,
        ..AlignmentCoverageEngineConfig::new(path, AlignmentOpenOptions::default(), ledger)
    }
}

fn whole(generation: u64, contig: u32) -> AlignmentCoverageRequest {
    AlignmentCoverageRequest {
        generation,
        contig: ContigId::new(contig),
        start: 0,
        end: MIXED_CONTIG_LENGTH,
        // 100 pixels: level-6 bins (65,536 bp) are the first at least a pixel wide.
        bases_per_pixel: 30_000.0,
        eligibility: CoverageEligibility::default(),
        exact_read_depth_ready: false,
    }
}

fn complete(publication: &AlignmentCoveragePublication) -> bool {
    matches!(
        publication.state,
        AlignmentCoverageState::Complete {
            missing_margin_units: 0
        }
    )
}

fn summary_charged(ledger: &AllocationLedger) -> usize {
    ledger
        .owner_usage(AllocationOwner::Summary)
        .unwrap()
        .charged
}

/// The tiles a fresh handle builds in one pass, with no engine involved.
fn direct_tiles(
    path: &Path,
    parameters: CoveragePyramidParameters,
    contig: u32,
    level: u8,
    contig_length: u64,
    eligibility: &CoverageEligibility,
) -> Vec<CoverageTile> {
    let mut source =
        ProductionAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
    let cancellation: RemoteCancellation = Arc::new(|| true);
    let outcome = source
        .coverage_tiles(
            &CoverageTileRequest {
                parameters,
                contig: ContigId::new(contig),
                level,
                tiles: parameters.tile_range(level, 0, contig_length, contig_length),
                eligibility: eligibility.clone(),
            },
            None,
            &cancellation,
        )
        .unwrap();
    match outcome {
        CoverageTileOutcome::Complete(production) => production.tiles,
        CoverageTileOutcome::Cancelled => panic!("cancelled"),
    }
}

fn lane_tiles(publication: &AlignmentCoveragePublication) -> Vec<CoverageTile> {
    publication
        .lane
        .as_ref()
        .unwrap()
        .segments()
        .iter()
        .map(|segment| segment.tile.tile().clone())
        .collect()
}

#[test]
fn whole_contig_coverage_is_built_from_bounded_units_and_persisted_for_the_next_session() {
    let directory = tempfile::tempdir().unwrap();
    let sidecar = Arc::new(CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap());
    let ledger = ledger();
    let config = AlignmentCoverageEngineConfig {
        sidecar: Some(Arc::clone(&sidecar)),
        ..small_config(mixed(), ledger.clone())
    };
    let parameters = config.parameters;
    let expected = direct_tiles(
        &mixed(),
        parameters,
        0,
        6,
        MIXED_CONTIG_LENGTH,
        &CoverageEligibility::default(),
    );
    assert_eq!(expected.len(), 1);
    assert!(expected[0].maximum_depth() > 0);

    {
        let engine = AlignmentCoverageEngine::spawn(config.clone()).unwrap();
        engine.request(OWNER, whole(41, 0));
        let publication = engine
            .wait_for_publication(OWNER, WAIT, complete)
            .expect("whole-contig coverage completes");
        assert_eq!(publication.generation, 41);
        assert_eq!(publication.owner, OWNER);
        assert_eq!(publication.authority, DepthLaneAuthority::Pyramid);
        let lane = publication.lane.as_ref().unwrap();
        assert_eq!(lane.target_level(), 6);
        assert!(lane.is_exact_at_target() && lane.covers_visible());
        // Twelve 262 kbp units reduced twice equal one pass over the whole contig.
        assert_eq!(lane_tiles(&publication), expected);

        assert!(engine.wait_until_idle(WAIT));
        let telemetry = engine.telemetry();
        assert!(telemetry.source_opened);
        assert_eq!(telemetry.units_decoded, 12);
        assert_eq!(telemetry.units_failed, 0);
        assert!(telemetry.records_streamed > 10_000);
        // Three level-5 tiles and the root.
        assert_eq!(telemetry.tiles_derived, 4);
        assert_eq!(telemetry.sidecar_tiles_stored, 12 + 4);
        assert_eq!(telemetry.sidecar_tiles_loaded, 0);
        // Progressive: one publication per arriving unit at least, the lane growing each time.
        assert!(telemetry.publications >= 12, "{}", telemetry.publications);
        // No unit held more than a few kilobytes of depth state, whatever the read count.
        assert!(telemetry.peak_unit_working_bytes < 64 * 1024);

        // Accounting is exact: the Summary owner is charged the resident tiles and nothing
        // else, and no other owner is touched.
        assert_eq!(telemetry.resident_tiles, 16);
        assert_eq!(summary_charged(&ledger), telemetry.resident_bytes);
        for owner in [
            AllocationOwner::Evidence,
            AllocationOwner::Presentation,
            AllocationOwner::Gpu,
        ] {
            assert_eq!(ledger.owner_usage(owner).unwrap().charged, 0);
        }
        drop(publication);
    }
    // The engine is gone: nothing stays charged.
    assert_eq!(summary_charged(&ledger), 0);
    assert_eq!(sidecar.stats().stored, 16);

    // A new session over the same file: whole-chromosome coverage without decoding anything.
    let engine = AlignmentCoverageEngine::spawn(config).unwrap();
    engine.request(OWNER, whole(1, 0));
    let publication = engine
        .wait_for_publication(OWNER, WAIT, complete)
        .expect("coverage is served from the sidecar");
    assert_eq!(lane_tiles(&publication), expected);
    assert!(engine.wait_until_idle(WAIT));
    let telemetry = engine.telemetry();
    assert_eq!(telemetry.units_decoded, 0);
    assert_eq!(telemetry.records_streamed, 0);
    assert_eq!(telemetry.sidecar_tiles_loaded, 1);
    assert_eq!(telemetry.tiles_derived, 0);
}

#[test]
fn a_zoom_in_shows_the_coarse_level_at_once_and_refines_to_the_target_level() {
    let ledger = ledger();
    let engine = AlignmentCoverageEngine::spawn(small_config(mixed(), ledger.clone())).unwrap();
    engine.request(OWNER, whole(1, 0));
    assert!(engine.wait_for_publication(OWNER, WAIT, complete).is_some());

    // 400 bp per pixel is level 3 (1,024 bp bins); none of its tiles exist yet.
    let closer = AlignmentCoverageRequest {
        generation: 2,
        start: 1_400_000,
        end: 1_600_000,
        bases_per_pixel: 400.0,
        ..whole(2, 0)
    };
    engine.request(OWNER, closer.clone());
    // Every publication for the new camera covers the view: first from the coarser tiles
    // already resident, then from the target level. The panel is never blank.
    let first = engine
        .wait_for_publication(OWNER, WAIT, |publication| publication.generation == 2)
        .unwrap();
    assert!(first.lane.as_ref().unwrap().covers_visible());
    let refined = engine
        .wait_for_publication(OWNER, WAIT, |publication| {
            publication.generation == 2 && complete(publication)
        })
        .unwrap();
    assert!(refined.sequence >= first.sequence);
    let lane = refined.lane.as_ref().unwrap();
    assert_eq!(lane.target_level(), 3);
    assert!(lane.is_exact_at_target());
    let (margin_start, margin_end) = lane.view().with_margin();
    assert_eq!((margin_start, margin_end), (1_300_000, 1_700_000));
    assert!(
        lane.segments()
            .iter()
            .all(|segment| segment.tile.tile().id().level == 3)
    );
    // The refined tiles equal a direct pass at that level.
    let expected = direct_tiles(
        &mixed(),
        CoveragePyramidParameters::new(16, 4, 64).unwrap(),
        0,
        3,
        MIXED_CONTIG_LENGTH,
        &CoverageEligibility::default(),
    );
    for tile in lane_tiles(&refined) {
        assert_eq!(tile, expected[usize::try_from(tile.id().index).unwrap()]);
    }
    // The coarse and the fine picture agree on the view's total coverage.
    let coarse = first.lane.as_ref().unwrap();
    if !coarse.is_exact_at_target() {
        let wide = |lane: &DepthPyramidLane| lane.aggregate(1_310_720, 1_638_400).unwrap();
        assert_eq!(wide(coarse).0.depth_sum, wide(lane).0.depth_sum);
        assert_eq!(wide(coarse).0.maximum_depth, wide(lane).0.maximum_depth);
    }
    assert!(engine.wait_until_idle(WAIT));
    assert_eq!(summary_charged(&ledger), engine.telemetry().resident_bytes);
}

#[test]
fn the_latest_request_of_an_owner_wins_and_owners_are_independent() {
    let engine = AlignmentCoverageEngine::spawn(small_config(mixed(), ledger())).unwrap();
    // A burst of cameras for one owner; only the last may be answered in the end.
    for generation in 1..=20 {
        engine.request(OWNER, whole(generation, u32::from(generation % 2 == 0)));
    }
    let other = SourceOwnerId::new(8);
    engine.request(other, whole(500, 0));
    let last = engine
        .wait_for_publication(OWNER, WAIT, |publication| {
            publication.generation == 20 && complete(publication)
        })
        .expect("the latest request is answered");
    assert_eq!(last.lane.as_ref().unwrap().view().contig, ContigId::new(1));
    let second = engine
        .wait_for_publication(other, WAIT, complete)
        .expect("another owner is served as well");
    assert_eq!(second.generation, 500);
    assert_eq!(
        second.lane.as_ref().unwrap().view().contig,
        ContigId::new(0)
    );
    assert!(engine.wait_until_idle(WAIT));
    // Nothing published for a superseded generation survives.
    assert_eq!(engine.publication(OWNER).unwrap().generation, 20);
    let telemetry = engine.telemetry();
    // Two contigs' worth of units at most: superseded cameras did not each cost a decode.
    assert!(telemetry.units_decoded <= 24, "{}", telemetry.units_decoded);

    // An identical request is not a new generation of work.
    let sequence = engine.publication_sequence();
    engine.request(OWNER, whole(20, 1));
    assert!(engine.wait_until_idle(WAIT));
    assert_eq!(engine.publication_sequence(), sequence);

    // Releasing an owner drops its publication and the tiles only it retained.
    engine.release(other);
    assert!(engine.publication(other).is_none());
    assert!(engine.publication(OWNER).is_some());
}

#[test]
fn read_evidence_stays_the_authority_and_the_pyramid_does_no_work_for_it() {
    let ledger = ledger();
    let engine = AlignmentCoverageEngine::spawn(small_config(mixed(), ledger.clone())).unwrap();
    engine.request(
        OWNER,
        AlignmentCoverageRequest {
            exact_read_depth_ready: true,
            start: 1_000_000,
            end: 1_001_000,
            bases_per_pixel: 1.0,
            ..whole(3, 0)
        },
    );
    let publication = engine.wait_for_publication(OWNER, WAIT, |_| true).unwrap();
    assert_eq!(publication.generation, 3);
    assert_eq!(publication.authority, DepthLaneAuthority::ExactReadEvidence);
    assert_eq!(
        publication.state,
        AlignmentCoverageState::DeferredToReadEvidence
    );
    assert!(publication.lane.is_none());
    assert!(engine.wait_until_idle(WAIT));
    let telemetry = engine.telemetry();
    assert!(!telemetry.source_opened);
    assert_eq!(telemetry.units_decoded, 0);
    assert_eq!(summary_charged(&ledger), 0);

    // The same camera once read evidence is no longer resident (a wide view): the pyramid
    // takes over for the same owner.
    engine.request(OWNER, whole(4, 0));
    let publication = engine.wait_for_publication(OWNER, WAIT, complete).unwrap();
    assert_eq!(publication.generation, 4);
    assert_eq!(publication.authority, DepthLaneAuthority::Pyramid);
}

/// Per-base depth from the existing one-shot query path, the read-level reference.
fn query_depth(path: &Path, length: u64, options: &QueryOptions) -> Vec<u32> {
    use rgv_hts::AlignmentSource as _;
    let mut source =
        ProductionAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
    let query = source
        .query(
            GenomicInterval::new(ContigId::new(0), 0, length).unwrap(),
            QueryOptions {
                coverage_bins: NonZeroUsize::new(usize::try_from(length).unwrap()).unwrap(),
                ..options.clone()
            },
            &|| true,
        )
        .unwrap();
    assert!(query.pileup.provenance.exact_depth_complete);
    query.pileup.bins.iter().map(|bin| bin.depth).collect()
}

#[test]
fn the_published_pyramid_agrees_with_read_level_depth_for_the_tracks_filter_settings() {
    const LENGTH: u64 = 20_000;
    let filtered = QueryOptions {
        include_secondary: true,
        filter: AlignmentFilter {
            minimum_mapping_quality: Some(20),
            excluded_flags: 0x400 | 0x200,
            ..AlignmentFilter::default()
        },
        ..QueryOptions::default()
    };
    for options in [QueryOptions::default(), filtered] {
        let depth = query_depth(&variants(), LENGTH, &options);
        assert!(depth.iter().any(|depth| *depth > 0));
        let engine = AlignmentCoverageEngine::spawn(AlignmentCoverageEngineConfig::new(
            variants(),
            AlignmentOpenOptions::default(),
            ledger(),
        ))
        .unwrap();
        engine.request(
            OWNER,
            AlignmentCoverageRequest {
                generation: 1,
                contig: ContigId::new(0),
                start: 0,
                end: LENGTH,
                bases_per_pixel: 10.0,
                eligibility: CoverageEligibility::from_query_options(&options),
                exact_read_depth_ready: false,
            },
        );
        let publication = engine.wait_for_publication(OWNER, WAIT, complete).unwrap();
        let lane = publication.lane.as_ref().unwrap();
        assert_eq!(lane.target_level(), 0);
        let bins = lane_tiles(&publication)
            .iter()
            .flat_map(|tile| tile.bins().to_vec())
            .collect::<Vec<_>>();
        let expected = depth
            .chunks(16)
            .map(|bases| CoverageBin {
                depth_sum: bases.iter().map(|depth| u64::from(*depth)).sum(),
                minimum_depth: *bases.iter().min().unwrap(),
                maximum_depth: *bases.iter().max().unwrap(),
            })
            .collect::<Vec<_>>();
        assert_eq!(bins, expected);
    }
}

#[test]
fn a_source_that_does_not_open_is_reported_not_retried_in_a_loop() {
    let engine = AlignmentCoverageEngine::spawn(small_config(
        PathBuf::from("/nonexistent/rgv-coverage-test.bam"),
        ledger(),
    ))
    .unwrap();
    engine.request(OWNER, whole(1, 0));
    let publication = engine
        .wait_for_publication(OWNER, WAIT, |publication| {
            matches!(publication.state, AlignmentCoverageState::Failed(_))
        })
        .expect("the failure is published");
    assert!(publication.lane.is_none());
    assert!(engine.wait_until_idle(WAIT));
    assert_eq!(engine.telemetry().publications, 1);

    // A contig the source does not have fails that owner only.
    let engine = AlignmentCoverageEngine::spawn(small_config(mixed(), ledger())).unwrap();
    engine.request(OWNER, whole(1, 9));
    assert!(
        engine
            .wait_for_publication(OWNER, WAIT, |publication| {
                matches!(publication.state, AlignmentCoverageState::Failed(_))
            })
            .is_some()
    );
}

fn probe(shared: &Arc<Shared>) -> Arc<UnitProbe> {
    let epoch = shared.epoch.load(Ordering::Acquire);
    Arc::new(UnitProbe {
        shared: Arc::clone(shared),
        started_epoch: epoch,
        checked_epoch: AtomicU64::new(epoch),
        wanted: AtomicBool::new(true),
        still_wanted: Box::new(|state| !state.requests.is_empty()),
        permit: Mutex::new(None),
        yields: AtomicU64::new(0),
    })
}

fn shared_with_limit(limit: usize) -> Arc<Shared> {
    Arc::new(Shared {
        state: Mutex::new(SharedState::default()),
        work: Condvar::new(),
        published: Condvar::new(),
        epoch: AtomicU64::new(0),
        shutdown: AtomicBool::new(false),
        allowance: Arc::new(DecodeAllowance::new(NonZeroUsize::new(limit).unwrap())),
    })
}

/// With every decode permit in use by coverage, a foreground read gets the permit at the
/// coverage unit's next probe (a few hundred records), not when the unit finishes.
#[test]
fn a_coverage_unit_hands_its_decode_permit_to_a_waiting_foreground_read() {
    let shared = shared_with_limit(1);
    let probe = probe(&shared);
    assert!(probe.acquire());
    assert_eq!(shared.allowance.snapshot().active, 1);
    // Nothing waiting: the unit keeps its permit.
    assert!(probe.keep_going());
    assert_eq!(probe.yields.load(Ordering::Relaxed), 0);

    let order = Arc::new(AtomicUsize::new(0));
    let foreground_turn = Arc::new(AtomicUsize::new(0));
    let foreground = {
        let shared = Arc::clone(&shared);
        let order = Arc::clone(&order);
        let foreground_turn = Arc::clone(&foreground_turn);
        std::thread::spawn(move || {
            let permit = shared
                .allowance
                .acquire(DecodeClass::Foreground, &|| true)
                .expect("the foreground read is served");
            foreground_turn.store(order.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            drop(permit);
        })
    };
    while shared.allowance.snapshot().waiting == 0 {
        std::thread::yield_now();
    }
    // The foreground read is blocked behind the coverage unit until the unit's next probe.
    assert_eq!(foreground_turn.load(Ordering::SeqCst), 0);
    assert!(probe.keep_going());
    let coverage_turn = order.fetch_add(1, Ordering::SeqCst) + 1;
    foreground.join().unwrap();
    // The foreground read ran first; the coverage unit resumed after it and goes on.
    assert_eq!(foreground_turn.load(Ordering::SeqCst), 1);
    assert_eq!(coverage_turn, 2);
    assert_eq!(probe.yields.load(Ordering::Relaxed), 1);
    let snapshot = shared.allowance.snapshot();
    assert_eq!(snapshot.foreground_grants, 1);
    assert_eq!(snapshot.speculative_grants, 2);
    assert_eq!(snapshot.active, 1);
    probe.release();
    assert_eq!(shared.allowance.snapshot().active, 0);
}

#[test]
fn a_unit_nobody_wants_any_more_stops_at_its_next_probe() {
    let shared = shared_with_limit(4);
    shared.lock().requests.insert(
        OWNER,
        OwnerRequest {
            request: whole(1, 0),
            arrival: 1,
        },
    );
    let probe = probe(&shared);
    assert!(probe.acquire());
    assert!(probe.keep_going());
    // An unrelated change: the unit is still wanted.
    let bump = |shared: &Arc<Shared>| {
        let mut state = shared.lock();
        state.epoch += 1;
        shared.epoch.store(state.epoch, Ordering::Release);
    };
    bump(&shared);
    assert!(probe.keep_going());
    // The request goes away (the camera moved elsewhere): the unit stops.
    shared.lock().requests.clear();
    bump(&shared);
    assert!(!probe.keep_going());
    assert!(!probe.keep_going());
    probe.release();

    // Shutdown stops a unit even while it waits for its decode turn.
    let shared = shared_with_limit(1);
    let holder = shared
        .allowance
        .acquire_owned(DecodeClass::Foreground, &|| true)
        .unwrap();
    let waiting = probe_waiting(&shared);
    while shared.allowance.snapshot().waiting == 0 {
        std::thread::yield_now();
    }
    shared.shutdown.store(true, Ordering::Release);
    shared.allowance.wake();
    assert!(!waiting.join().unwrap());
    drop(holder);
    assert_eq!(shared.allowance.snapshot().active, 0);
}

fn probe_waiting(shared: &Arc<Shared>) -> std::thread::JoinHandle<bool> {
    let probe = probe(shared);
    std::thread::spawn(move || probe.acquire())
}

/// A SAM file of one amplicon: `templates` pairs whose first mates all cover `[1000, 1020)`
/// and whose second mates all cover `[1010, 1030)`.
fn amplicon_sam(directory: &Path, templates: usize) -> PathBuf {
    use std::fmt::Write as _;
    let mut text = String::from("@HD\tVN:1.6\tSO:coordinate\n@SQ\tSN:amplicon\tLN:100000\n");
    for (flags, position, mate, length) in [(99, 1_001, 1_011, 30), (147, 1_011, 1_001, -30)] {
        for template in 0..templates {
            writeln!(
                text,
                "t{template}\t{flags}\tamplicon\t{position}\t60\t20M\t=\t{mate}\t{length}\t*\t*"
            )
            .unwrap();
        }
    }
    let path = directory.join("amplicon.sam");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn an_ultra_deep_amplicon_gets_pyramid_coverage_flagged_as_mate_overlap_not_collapsed() {
    let directory = tempfile::tempdir().unwrap();
    // More first mates wait for theirs at once than the count-once window holds.
    let templates = rgv_model::pileup::MAX_PENDING_PAIR_OVERLAPS + 20_000;
    let path = amplicon_sam(directory.path(), templates);
    let sidecar =
        Arc::new(CoverageSidecarCache::open(directory.path().join("cache"), 1 << 30).unwrap());
    let engine = AlignmentCoverageEngine::spawn(AlignmentCoverageEngineConfig {
        sidecar: Some(Arc::clone(&sidecar)),
        ..AlignmentCoverageEngineConfig::new(path, AlignmentOpenOptions::default(), ledger())
    })
    .unwrap();
    let request = AlignmentCoverageRequest {
        generation: 1,
        contig: ContigId::new(0),
        start: 0,
        end: 100_000,
        bases_per_pixel: 100.0,
        eligibility: CoverageEligibility::default(),
        exact_read_depth_ready: false,
    };
    engine.request(OWNER, request.clone());
    let publication = engine
        .wait_for_publication(OWNER, WAIT, |publication| {
            !matches!(publication.state, AlignmentCoverageState::Refining { .. })
        })
        .expect("the amplicon is answered");
    // Coverage, not a refusal: every mate counted, and the publication says so.
    assert!(
        matches!(publication.state, AlignmentCoverageState::Complete { .. }),
        "{:?}",
        publication.state
    );
    assert!(publication.mate_overlap_not_collapsed);
    let lane = publication.lane.as_ref().unwrap();
    let depth = u32::try_from(templates).unwrap();
    assert_eq!(lane.maximum_depth(0, 100_000), 2 * depth);
    assert_eq!(lane.maximum_depth(50_000, 60_000), 0);
    assert!(engine.wait_until_idle(WAIT));
    let telemetry = engine.telemetry();
    assert_eq!(telemetry.units_failed, 0);
    assert_eq!(telemetry.units_mate_overlap_not_collapsed, 1);
    assert_eq!(telemetry.units_decoded, 1);
    // A degraded tile is never persisted under the count-once key.
    assert_eq!(telemetry.sidecar_tiles_stored, 0);
    assert_eq!(sidecar.stats().stored, 0);

    // Asked to count both mates, the same file is exact and carries no flag.
    engine.request(
        OWNER,
        AlignmentCoverageRequest {
            generation: 2,
            eligibility: CoverageEligibility {
                overlapping_mate_policy: rgv_core::OverlappingMatePolicy::CountBoth,
                ..CoverageEligibility::default()
            },
            ..request
        },
    );
    let publication = engine
        .wait_for_publication(OWNER, WAIT, |publication| {
            publication.generation == 2
                && !matches!(publication.state, AlignmentCoverageState::Refining { .. })
        })
        .unwrap();
    assert!(matches!(
        publication.state,
        AlignmentCoverageState::Complete { .. }
    ));
    assert!(!publication.mate_overlap_not_collapsed);
    assert_eq!(
        publication.lane.as_ref().unwrap().maximum_depth(0, 100_000),
        2 * depth
    );
}

#[test]
fn retiring_or_dropping_the_engine_never_waits_for_the_unit_in_flight() {
    let source = mixed();
    if !source.exists() {
        return;
    }
    let ledger = ledger();
    for retire in [true, false] {
        let engine =
            AlignmentCoverageEngine::spawn(small_config(source.clone(), ledger.clone())).unwrap();
        engine.request(OWNER, whole(1, 0));
        // Work is in flight (or about to be): twelve units cannot all be done yet.
        let started = Instant::now();
        let reaper = if retire {
            engine.retire()
        } else {
            drop(engine);
            None
        };
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stopping the engine blocked for {:?}",
            started.elapsed()
        );
        if let Some(reaper) = reaper {
            reaper.join().unwrap();
            // The worker is gone, and with it every resident tile and its charge.
            assert_eq!(summary_charged(&ledger), 0);
        }
    }
    // The dropped engine's worker stops too, one probe later at most.
    let deadline = Instant::now() + WAIT;
    while summary_charged(&ledger) != 0 {
        assert!(
            Instant::now() < deadline,
            "the dropped engine never stopped"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
