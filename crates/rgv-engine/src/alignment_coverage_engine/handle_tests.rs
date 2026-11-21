//! The coverage engine's source handle: opened per decode burst, closed when idle, never
//! opened for a request the sidecar serves when the caller states the source's identity, and
//! a rewritten source rebuilds coverage instead of mixing two revisions.

use std::{path::Path, sync::atomic::AtomicUsize};

use rgv_alignment_scene::AlignmentMemoryPolicy;

use super::*;

const WAIT: Duration = Duration::from_secs(120);
const OWNER: SourceOwnerId = SourceOwnerId::new(11);
const MIXED_CONTIG_LENGTH: u64 = 3_000_000;
const STAMP: AlignmentSourceStamp = AlignmentSourceStamp::new([7; 16], [1; 16]);
const REWRITTEN: AlignmentSourceStamp = AlignmentSourceStamp::new([7; 16], [2; 16]);

fn mixed() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam")
}

fn ledger() -> AllocationLedger {
    AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, 1 << 30, 1 << 30, 1 << 30)
}

fn small_config(ledger: AllocationLedger) -> AlignmentCoverageEngineConfig {
    AlignmentCoverageEngineConfig {
        parameters: CoveragePyramidParameters::new(16, 4, 64).unwrap(),
        production_unit_bases: 262_144,
        source_idle_close: Duration::from_millis(40),
        ..AlignmentCoverageEngineConfig::new(mixed(), AlignmentOpenOptions::default(), ledger)
    }
}

fn whole(generation: u64) -> AlignmentCoverageRequest {
    AlignmentCoverageRequest {
        generation,
        contig: ContigId::new(0),
        start: 0,
        end: MIXED_CONTIG_LENGTH,
        bases_per_pixel: 30_000.0,
        eligibility: CoverageEligibility::default(),
        exact_read_depth_ready: false,
    }
}

/// 400 bp per pixel over 200 kbp: level-3 tiles, none of which a whole-contig view makes.
fn closer(generation: u64) -> AlignmentCoverageRequest {
    AlignmentCoverageRequest {
        start: 1_400_000,
        end: 1_600_000,
        bases_per_pixel: 400.0,
        ..whole(generation)
    }
}

fn complete(generation: u64) -> impl Fn(&AlignmentCoveragePublication) -> bool {
    move |publication| {
        publication.generation == generation
            && matches!(
                publication.state,
                AlignmentCoverageState::Complete {
                    missing_margin_units: 0
                }
            )
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

/// Opens the real source and counts every open and every close (drop) of a handle. The
/// stamp it reports can be changed between opens, as a rewritten file would change it.
#[derive(Clone, Default)]
struct Counting {
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    stamp: Arc<Mutex<Option<AlignmentSourceStamp>>>,
    remote: bool,
}

impl Counting {
    fn with_stamp(stamp: AlignmentSourceStamp) -> Self {
        Self {
            stamp: Arc::new(Mutex::new(Some(stamp))),
            ..Self::default()
        }
    }

    fn opens(&self) -> usize {
        self.opens.load(Ordering::Acquire)
    }

    fn closes(&self) -> usize {
        self.closes.load(Ordering::Acquire)
    }

    fn held(&self) -> usize {
        self.opens() - self.closes()
    }

    fn rewrite(&self, stamp: AlignmentSourceStamp) {
        *self.stamp.lock().unwrap() = Some(stamp);
    }

    fn spawn(&self, config: AlignmentCoverageEngineConfig) -> AlignmentCoverageEngine {
        AlignmentCoverageEngine::spawn_with_opener(
            config,
            DecodeAllowance::process(),
            Box::new(self.clone()),
        )
        .unwrap()
    }
}

struct CountingHandle {
    source: ProductionAlignmentSource,
    stamp: Option<AlignmentSourceStamp>,
    remote: bool,
    closes: Arc<AtomicUsize>,
}

impl Drop for CountingHandle {
    fn drop(&mut self) {
        self.closes.fetch_add(1, Ordering::AcqRel);
    }
}

impl CoverageSourceOpener for Counting {
    fn open(
        &self,
        path: &Path,
        options: &AlignmentOpenOptions,
    ) -> Result<Box<dyn CoverageSourceHandle>, String> {
        let source =
            ProductionAlignmentSource::open(path, options).map_err(|error| error.to_string())?;
        self.opens.fetch_add(1, Ordering::AcqRel);
        let stamp = self.stamp.lock().unwrap().or_else(|| source.source_stamp());
        Ok(Box::new(CountingHandle {
            source,
            stamp,
            remote: self.remote,
            closes: Arc::clone(&self.closes),
        }))
    }
}

impl CoverageSourceHandle for CountingHandle {
    fn dictionary(&self) -> &ReferenceDictionary {
        self.source.reference_dictionary()
    }

    fn stamp(&self) -> Option<AlignmentSourceStamp> {
        self.stamp
    }

    fn is_remote(&self) -> bool {
        self.remote
    }

    fn coverage_tiles(
        &mut self,
        request: &CoverageTileRequest,
        transfer_budget: Option<RemoteTransferBudget>,
        cancellation: &RemoteCancellation,
    ) -> Result<CoverageTileOutcome, String> {
        self.source
            .coverage_tiles(request, transfer_budget, cancellation)
            .map_err(|error| error.to_string())
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "the condition never held");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn mixed_dictionary() -> ReferenceDictionary {
    ProductionAlignmentSource::open(mixed(), &AlignmentOpenOptions::default())
        .unwrap()
        .reference_dictionary()
        .clone()
}

#[test]
fn an_idle_engine_holds_no_source_handle_and_reopens_for_the_next_burst() {
    let counting = Counting::default();
    let engine = counting.spawn(small_config(ledger()));
    // Spawned and asked nothing: nothing is open.
    assert!(engine.wait_until_idle(WAIT));
    assert_eq!(counting.opens(), 0);

    engine.request(OWNER, whole(1));
    assert!(
        engine
            .wait_for_publication(OWNER, WAIT, complete(1))
            .is_some()
    );
    assert!(engine.wait_until_idle(WAIT));
    // Twelve units in one burst through one handle.
    assert_eq!(engine.telemetry().units_decoded, 12);
    assert_eq!(counting.opens(), 1);
    // Idle: the handle is closed after the grace period, and the engine says so.
    wait_until(|| counting.held() == 0);
    wait_until(|| !engine.telemetry().source_handle_open);
    let telemetry = engine.telemetry();
    assert_eq!((telemetry.source_opens, telemetry.source_closes), (1, 1));
    assert!(telemetry.source_opened);

    // The next request that needs decoding reopens once for its burst, and closes again.
    engine.request(OWNER, closer(2));
    assert!(
        engine
            .wait_for_publication(OWNER, WAIT, complete(2))
            .is_some()
    );
    assert!(engine.wait_until_idle(WAIT));
    assert_eq!(counting.opens(), 2);
    wait_until(|| counting.held() == 0);
    // The same revision: nothing was invalidated by the reopen.
    assert_eq!(engine.telemetry().source_revision_changes, 0);

    // A request served from resident tiles opens nothing.
    engine.request(OWNER, whole(3));
    assert!(
        engine
            .wait_for_publication(OWNER, WAIT, complete(3))
            .is_some()
    );
    assert!(engine.wait_until_idle(WAIT));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!((counting.opens(), counting.held()), (2, 0));
    drop(engine);
}

#[test]
fn a_remote_source_keeps_its_idle_handle_for_its_longer_grace_period() {
    let counting = Counting {
        remote: true,
        ..Counting::default()
    };
    let config = AlignmentCoverageEngineConfig {
        remote_source_idle_close: Duration::from_secs(3_600),
        ..small_config(ledger())
    };
    let engine = counting.spawn(config);
    engine.request(OWNER, whole(1));
    assert!(
        engine
            .wait_for_publication(OWNER, WAIT, complete(1))
            .is_some()
    );
    assert!(engine.wait_until_idle(WAIT));
    // Well past the local grace period, the remote handle is still held.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!((counting.opens(), counting.held()), (1, 1));
    assert!(engine.telemetry().source_handle_open);
    // Retiring the engine releases it.
    engine.retire().unwrap().join().unwrap();
    assert_eq!(counting.held(), 0);
}

#[test]
fn a_request_the_sidecar_serves_opens_nothing_when_the_dictionary_and_stamp_are_known() {
    let directory = tempfile::tempdir().unwrap();
    let sidecar = Arc::new(CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap());
    let config = AlignmentCoverageEngineConfig {
        sidecar: Some(Arc::clone(&sidecar)),
        ..small_config(ledger())
    };
    // An earlier session built and persisted the whole contig.
    let first = Counting::with_stamp(STAMP);
    let expected = {
        let engine = first.spawn(config.clone());
        engine.request(OWNER, whole(1));
        let publication = engine
            .wait_for_publication(OWNER, WAIT, complete(1))
            .unwrap();
        assert!(engine.wait_until_idle(WAIT));
        lane_tiles(&publication)
    };
    assert_eq!(sidecar.stats().stored, 16);

    // Without the identity the engine must open the source to learn it, sidecar or not.
    let unknown = Counting::with_stamp(STAMP);
    {
        let engine = unknown.spawn(config.clone());
        engine.request(OWNER, whole(1));
        let publication = engine
            .wait_for_publication(OWNER, WAIT, complete(1))
            .unwrap();
        assert_eq!(lane_tiles(&publication), expected);
        assert!(engine.wait_until_idle(WAIT));
        assert_eq!(engine.telemetry().units_decoded, 0);
    }
    assert_eq!(unknown.opens(), 1);

    // With the dictionary and stamp stated, the same request performs zero opens.
    let known = Counting::with_stamp(STAMP);
    let engine = known.spawn(config.with_known_source(mixed_dictionary(), Some(STAMP)));
    engine.request(OWNER, whole(2));
    let publication = engine
        .wait_for_publication(OWNER, WAIT, complete(2))
        .unwrap();
    assert_eq!(lane_tiles(&publication), expected);
    assert!(engine.wait_until_idle(WAIT));
    let telemetry = engine.telemetry();
    assert_eq!(known.opens(), 0);
    assert_eq!((telemetry.source_opens, telemetry.units_decoded), (0, 0));
    assert!(!telemetry.source_opened && !telemetry.source_handle_open);
    assert_eq!(telemetry.sidecar_tiles_loaded, 1);
}

#[test]
fn a_source_rewritten_between_bursts_is_rebuilt_rather_than_mixed_with_the_old_revision() {
    let directory = tempfile::tempdir().unwrap();
    let sidecar = Arc::new(CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap());
    let config = AlignmentCoverageEngineConfig {
        sidecar: Some(Arc::clone(&sidecar)),
        ..small_config(ledger())
    };
    let parameters = config.parameters;
    let eligibility = CoverageEligibility::default();
    let old_key = CoverageSidecarKey::new(STAMP, &eligibility, parameters);
    let new_key = CoverageSidecarKey::new(REWRITTEN, &eligibility, parameters);
    let counting = Counting::with_stamp(STAMP);
    let engine = counting.spawn(config);

    // Revision 1: the whole contig, decoded and persisted under its stamp.
    engine.request(OWNER, whole(1));
    let first = engine
        .wait_for_publication(OWNER, WAIT, complete(1))
        .unwrap();
    let expected = lane_tiles(&first);
    drop(first);
    assert!(engine.wait_until_idle(WAIT));
    wait_until(|| counting.held() == 0);
    let before = engine.telemetry();
    assert_eq!((before.units_decoded, before.resident_tiles), (12, 16));

    // The file is rewritten while the engine is idle and holds no handle.
    counting.rewrite(REWRITTEN);
    engine.request(OWNER, closer(2));
    let zoomed = engine
        .wait_for_publication(OWNER, WAIT, complete(2))
        .unwrap();
    assert!(engine.wait_until_idle(WAIT));
    let after_zoom = engine.telemetry();
    assert_eq!(after_zoom.source_revision_changes, 1);
    assert_eq!(counting.opens(), 2);
    // Every tile the new revision made went to the new key, none to the old one.
    let zoomed_tiles = lane_tiles(&zoomed);
    assert!(!zoomed_tiles.is_empty());
    for tile in &zoomed_tiles {
        let id = tile.id();
        assert_eq!(
            sidecar
                .load(new_key, id, parameters, MIXED_CONTIG_LENGTH)
                .as_ref(),
            Some(tile)
        );
        assert!(
            sidecar
                .load(old_key, id, parameters, MIXED_CONTIG_LENGTH)
                .is_none()
        );
    }
    // Only what the zoomed view needed is resident: the old revision's tiles are gone.
    assert!(after_zoom.resident_tiles < before.resident_tiles + zoomed_tiles.len());

    // Back to the whole contig: the old revision's root (resident and persisted under the
    // old key) is not reused; the contig is decoded again under the new revision.
    engine.request(OWNER, whole(3));
    let rebuilt = engine
        .wait_for_publication(OWNER, WAIT, complete(3))
        .unwrap();
    assert!(engine.wait_until_idle(WAIT));
    let after = engine.telemetry();
    assert!(
        after.units_decoded > after_zoom.units_decoded,
        "the old revision's tiles stood in: {after:?}"
    );
    assert_eq!(
        after.sidecar_tiles_loaded, 0,
        "an old-revision tile was loaded"
    );
    assert_eq!(after.source_revision_changes, 1);
    // The data is the file's (unchanged here), now under the new revision's key.
    assert_eq!(lane_tiles(&rebuilt), expected);
    let root = rebuilt.lane.as_ref().unwrap().segments()[0]
        .tile
        .tile()
        .id();
    assert!(
        sidecar
            .load(new_key, root, parameters, MIXED_CONTIG_LENGTH)
            .is_some()
    );
}

#[test]
fn a_stated_stamp_the_source_contradicts_is_dropped_at_the_first_decode() {
    let directory = tempfile::tempdir().unwrap();
    let sidecar = Arc::new(CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap());
    let config = AlignmentCoverageEngineConfig {
        sidecar: Some(Arc::clone(&sidecar)),
        ..small_config(ledger())
    };
    // The caller states revision 1, but the file on disk is revision 2.
    let counting = Counting::with_stamp(REWRITTEN);
    let engine = counting.spawn(config.with_known_source(mixed_dictionary(), Some(STAMP)));
    engine.request(OWNER, whole(1));
    assert!(
        engine
            .wait_for_publication(OWNER, WAIT, complete(1))
            .is_some()
    );
    assert!(engine.wait_until_idle(WAIT));
    let telemetry = engine.telemetry();
    assert_eq!(telemetry.source_revision_changes, 1);
    assert_eq!(telemetry.units_decoded, 12);
    // Persisted under the revision actually read.
    let parameters = CoveragePyramidParameters::new(16, 4, 64).unwrap();
    let eligibility = CoverageEligibility::default();
    let stored = |stamp| {
        let key = CoverageSidecarKey::new(stamp, &eligibility, parameters);
        sidecar
            .load(
                key,
                CoverageTileId::new(ContigId::new(0), 6, 0),
                parameters,
                MIXED_CONTIG_LENGTH,
            )
            .is_some()
    };
    assert!(stored(REWRITTEN));
    assert!(!stored(STAMP));
}
