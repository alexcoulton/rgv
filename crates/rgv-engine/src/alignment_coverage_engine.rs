//! Coverage-pyramid engine: multi-resolution exact coverage for one alignment source, produced
//! and published independently of read evidence.
//!
//! One engine serves one track. Each panel (owner) states its camera; the engine publishes, per
//! owner, the best coverage it has for that camera and keeps improving it:
//!
//! 1. **Select** the pyramid level whose bins are at least one pixel wide.
//! 2. **Obtain without decoding** every tile of that level under the view and its margin: from
//!    the resident store, from the sidecar cache, or by reducing finer tiles that are.
//! 3. **Publish** what is there. A coarser ancestor stands in where the target level is
//!    missing, so a zoom-in shows coverage at once and sharpens.
//! 4. **Decode one unit** that is still missing (visible first, then margin, nearest first)
//!    with the streaming producer, store it, persist it, and go back to 1.
//!
//! # Not in the way of reads
//!
//! The engine has its own source handle and its own thread, so it never touches the scene
//! worker's positioned stream. It decodes under the process-wide decode allowance as
//! [`DecodeClass::Speculative`], the class below foreground reads, and its cancellation probe
//! (polled every few hundred records) hands the permit to a waiting foreground unit and queues
//! again behind it.
//!
//! # Latest wins
//!
//! Every request carries the caller's generation token. A publication names the generation it
//! answers and is dropped if the owner has asked again meanwhile. A unit in flight is cancelled
//! as soon as no current request wants it.
//!
//! # Authority and memory
//!
//! A request that reports exact read-derived depth as ready is answered with
//! [`DepthLaneAuthority::ExactReadEvidence`] and schedules no work. Resident tiles are charged
//! to the shared ledger's Summary owner by [`DepthPyramidStore`], within its own cap.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rgv_alignment_scene::{
    AllocationLedger, DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH, DEPTH_PYRAMID_PRODUCTION_UNIT_BASES,
    DepthLaneAuthority, DepthPyramidLane, DepthPyramidStore, DepthPyramidTile, DepthPyramidView,
};
use rgv_core::{ContigId, ReferenceDictionary};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentSource as _, CoverageEligibility, CoveragePyramidParameters,
    CoverageSidecarCache, CoverageSidecarKey, CoverageTile, CoverageTileId, CoverageTileOutcome,
    CoverageTileRequest, ProductionAlignmentSource, RemoteCancellation, RemoteTransferBudget,
};

use crate::{
    SourceOwnerId,
    decode_allowance::{DecodeAllowance, DecodeClass, OwnedDecodePermit},
};

/// Default cap of resident pyramid tiles for one source: 16 MiB, about a thousand tiles.
pub const ALIGNMENT_COVERAGE_DEFAULT_RESIDENT_BYTES: usize = 16 << 20;

/// Most one decode unit may transfer from a remote object before it fails with a clear error.
pub const ALIGNMENT_COVERAGE_REMOTE_UNIT_TRANSFER_BUDGET: RemoteTransferBudget =
    RemoteTransferBudget {
        maximum_requests: 20_000,
        maximum_response_bytes: 2 << 30,
    };

/// Everything needed to serve coverage for one alignment source.
#[derive(Clone, Debug)]
pub struct AlignmentCoverageEngineConfig {
    pub path: PathBuf,
    pub open_options: AlignmentOpenOptions,
    /// The workspace's shared ledger; resident tiles are charged to its Summary owner.
    pub ledger: AllocationLedger,
    /// Persistent tile cache, shared by every source. `None` keeps coverage in memory only.
    pub sidecar: Option<Arc<CoverageSidecarCache>>,
    pub parameters: CoveragePyramidParameters,
    pub resident_byte_limit: usize,
    /// Widest genomic span one decode unit covers; see
    /// [`rgv_alignment_scene::DEPTH_PYRAMID_PRODUCTION_UNIT_BASES`].
    pub production_unit_bases: u64,
    /// Transfer bound of one decode unit of a remote source.
    pub remote_unit_transfer_budget: RemoteTransferBudget,
}

impl AlignmentCoverageEngineConfig {
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        open_options: AlignmentOpenOptions,
        ledger: AllocationLedger,
    ) -> Self {
        Self {
            path: path.into(),
            open_options,
            ledger,
            sidecar: None,
            parameters: CoveragePyramidParameters::DEFAULT,
            resident_byte_limit: ALIGNMENT_COVERAGE_DEFAULT_RESIDENT_BYTES,
            production_unit_bases: DEPTH_PYRAMID_PRODUCTION_UNIT_BASES,
            remote_unit_transfer_budget: ALIGNMENT_COVERAGE_REMOTE_UNIT_TRANSFER_BUDGET,
        }
    }
}

/// One owner's camera and settings. A later request of the same owner replaces it.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentCoverageRequest {
    /// The caller's generation token; echoed by the publication that answers this request.
    pub generation: u64,
    pub contig: ContigId,
    /// Visible half-open base span.
    pub start: u64,
    pub end: u64,
    pub bases_per_pixel: f64,
    /// Filter settings of the track; the same ones the read-level path uses.
    pub eligibility: CoverageEligibility,
    /// Exact read-derived depth is resident and current for the visible interval. It is then
    /// the authority and the pyramid does no work for this owner.
    pub exact_read_depth_ready: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlignmentCoverageState {
    /// Read evidence is the authority for this view; the lane is not needed.
    DeferredToReadEvidence,
    /// Tiles are being loaded or decoded; the lane shows what is resident so far.
    Refining {
        missing_visible_units: usize,
        missing_margin_units: usize,
    },
    /// The visible span is shown from the target level; only margin work may remain.
    Complete { missing_margin_units: usize },
    /// The source could not be opened, or a unit the view needs could not be produced.
    Failed(Arc<str>),
}

/// The coverage the engine currently has for one owner's latest request.
#[derive(Clone, Debug)]
pub struct AlignmentCoveragePublication {
    pub owner: SourceOwnerId,
    /// Generation token of the request this answers.
    pub generation: u64,
    /// Engine-wide publication counter; grows with every publication.
    pub sequence: u64,
    pub authority: DepthLaneAuthority,
    /// Present whenever the pyramid is the authority, possibly with no segment yet.
    pub lane: Option<Arc<DepthPyramidLane>>,
    pub state: AlignmentCoverageState,
    /// Some of the coverage under the visible span counts every mate independently although
    /// the request asked for overlapping mates to be counted once: the region is so deep that
    /// the count-once window overflowed (see
    /// [`rgv_hts::CoverageTileProduction::mate_overlap_not_collapsed`]). The lane is still
    /// exact for what it counts; where two mates of a template overlap it is one higher than
    /// the count-once depth. The UI should say so (for instance "mate overlap not
    /// collapsed"). Such tiles are kept in memory only, never in the sidecar cache.
    pub mate_overlap_not_collapsed: bool,
}

/// Counters of one engine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentCoverageTelemetry {
    /// Units decoded from the source.
    pub units_decoded: u64,
    /// Units abandoned because no current request wanted them any more.
    pub units_cancelled: u64,
    pub units_failed: u64,
    /// Records the decoded units streamed.
    pub records_streamed: u64,
    /// Tiles taken from the sidecar cache instead of decoding.
    pub sidecar_tiles_loaded: u64,
    pub sidecar_tiles_stored: u64,
    /// Coarser tiles derived by reducing finer ones.
    pub tiles_derived: u64,
    pub publications: u64,
    /// Publications dropped because the owner had asked again.
    pub superseded_publications: u64,
    /// Times a unit gave its decode permit to a waiting foreground unit.
    pub foreground_yields: u64,
    pub resident_tiles: usize,
    pub resident_bytes: usize,
    /// Largest depth-state working set of any unit, in bytes.
    pub peak_unit_working_bytes: usize,
    pub source_opened: bool,
    /// Units decoded with every mate counted independently because the count-once window
    /// overflowed; also counted in `units_decoded`.
    pub units_mate_overlap_not_collapsed: u64,
}

struct OwnerRequest {
    request: AlignmentCoverageRequest,
    /// Order of arrival across owners; the most recent owner is served first.
    arrival: u64,
}

#[derive(Default)]
struct SharedState {
    requests: HashMap<SourceOwnerId, OwnerRequest>,
    publications: HashMap<SourceOwnerId, Arc<AlignmentCoveragePublication>>,
    /// Bumped by every request change.
    epoch: u64,
    arrivals: u64,
    sequence: u64,
    telemetry: AlignmentCoverageTelemetry,
    retry: u64,
    shutdown: bool,
    idle: bool,
}

struct Shared {
    state: Mutex<SharedState>,
    work: Condvar,
    published: Condvar,
    /// Mirrors `SharedState::epoch` and `shutdown` for the lock-free part of the probe.
    epoch: AtomicU64,
    shutdown: AtomicBool,
    allowance: Arc<DecodeAllowance>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SharedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Handle of one source's coverage worker. Dropping it stops the worker.
pub struct AlignmentCoverageEngine {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for AlignmentCoverageEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AlignmentCoverageEngine")
            .finish_non_exhaustive()
    }
}

impl AlignmentCoverageEngine {
    /// Starts the worker. The source is opened on the worker thread when the first request
    /// needs it, never on the caller's.
    pub fn spawn(config: AlignmentCoverageEngineConfig) -> std::io::Result<Self> {
        Self::spawn_with_allowance(config, DecodeAllowance::process())
    }

    pub(crate) fn spawn_with_allowance(
        config: AlignmentCoverageEngineConfig,
        allowance: Arc<DecodeAllowance>,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(SharedState::default()),
            work: Condvar::new(),
            published: Condvar::new(),
            epoch: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            allowance,
        });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("rgv-alignment-coverage".to_owned())
            .spawn(move || Worker::new(config, worker_shared).run())?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// States `owner`'s camera. Replaces the owner's previous request; never blocks on work.
    pub fn request(&self, owner: SourceOwnerId, request: AlignmentCoverageRequest) {
        let mut state = self.shared.lock();
        if state
            .requests
            .get(&owner)
            .is_some_and(|current| current.request == request)
        {
            return;
        }
        state.arrivals += 1;
        let arrival = state.arrivals;
        state
            .requests
            .insert(owner, OwnerRequest { request, arrival });
        self.changed(state);
    }

    /// Forgets `owner`: its request, its publication and the lane that publication retains.
    pub fn release(&self, owner: SourceOwnerId) {
        let mut state = self.shared.lock();
        let had_request = state.requests.remove(&owner).is_some();
        let had_publication = state.publications.remove(&owner).is_some();
        if had_request || had_publication {
            self.changed(state);
        }
    }

    /// Asks again for units that failed (a refused transfer budget, an interrupted network).
    pub fn retry_failed_units(&self) {
        let mut state = self.shared.lock();
        state.retry += 1;
        self.changed(state);
    }

    fn changed(&self, mut state: MutexGuard<'_, SharedState>) {
        state.epoch += 1;
        state.idle = false;
        self.shared.epoch.store(state.epoch, Ordering::Release);
        drop(state);
        self.shared.work.notify_all();
        // A unit waiting for its decode turn must notice that it may no longer be wanted.
        self.shared.allowance.wake();
    }

    /// The latest publication for `owner`, if any. Cheap: one lock and one `Arc` clone.
    #[must_use]
    pub fn publication(&self, owner: SourceOwnerId) -> Option<Arc<AlignmentCoveragePublication>> {
        self.shared.lock().publications.get(&owner).cloned()
    }

    /// Engine-wide publication counter, to notice new publications without comparing them.
    #[must_use]
    pub fn publication_sequence(&self) -> u64 {
        self.shared.lock().sequence
    }

    #[must_use]
    pub fn telemetry(&self) -> AlignmentCoverageTelemetry {
        self.shared.lock().telemetry
    }

    /// Blocks until `owner` has a publication that satisfies `accept`, or `timeout` passes.
    /// For headless callers (snapshot tools, tests); the desktop observes
    /// [`Self::publication`] when the UI waker fires.
    #[must_use]
    pub fn wait_for_publication(
        &self,
        owner: SourceOwnerId,
        timeout: Duration,
        accept: impl Fn(&AlignmentCoveragePublication) -> bool,
    ) -> Option<Arc<AlignmentCoveragePublication>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if let Some(publication) = state.publications.get(&owner)
                && accept(publication)
            {
                return Some(Arc::clone(publication));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            state = self
                .shared
                .published
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Blocks until the worker has nothing left to do for the current requests.
    #[must_use]
    pub fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if state.idle {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            state = self
                .shared
                .published
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl AlignmentCoverageEngine {
    /// Tells the worker to stop: the unit in flight is cancelled at its next probe (every few
    /// hundred records, and every range request of a remote source).
    fn signal_shutdown(&self) {
        {
            let mut state = self.shared.lock();
            state.shutdown = true;
            state.epoch += 1;
            self.shared.epoch.store(state.epoch, Ordering::Release);
        }
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.work.notify_all();
        self.shared.allowance.wake();
    }

    /// Stops the engine without waiting for it, like [`crate::TrackEngine::retire`]: the
    /// worker is told to stop and its join is handed to a reaper thread, whose handle is
    /// returned for callers that want to observe the exit. `None` when the worker had already
    /// finished. The source handle, the resident tiles and their ledger charge are released
    /// when the worker exits, which is at the latest one cancellation probe away.
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.signal_shutdown();
        let worker = self.worker.take()?;
        crate::mailbox::reap_worker(worker, "rgv-coverage-reaper", "alignment coverage worker")
    }
}

impl Drop for AlignmentCoverageEngine {
    /// Signals the worker exactly as [`Self::retire`] does and returns without joining it: an
    /// engine dropped on the UI thread must not wait for the decode or transfer in flight. A
    /// reaper thread waits for the worker instead.
    fn drop(&mut self) {
        self.signal_shutdown();
        if let Some(worker) = self.worker.take() {
            drop(crate::mailbox::reap_worker(
                worker,
                "rgv-coverage-reaper",
                "alignment coverage worker",
            ));
        }
    }
}

/// Cancellation probe of one decode unit. Also where the unit gives way to foreground reads.
struct UnitProbe {
    shared: Arc<Shared>,
    started_epoch: u64,
    checked_epoch: AtomicU64,
    wanted: AtomicBool,
    /// Decides, under the state lock, whether any current request still wants the unit.
    still_wanted: Box<dyn Fn(&SharedState) -> bool + Send + Sync>,
    permit: Mutex<Option<OwnedDecodePermit>>,
    yields: AtomicU64,
}

impl UnitProbe {
    fn stopped(&self) -> bool {
        self.shared.shutdown.load(Ordering::Acquire)
    }

    /// Whether the unit should go on. Cheap while nothing changed: two atomic loads and one
    /// short lock of the allowance queue.
    fn keep_going(&self) -> bool {
        if self.stopped() {
            return false;
        }
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        if epoch != self.started_epoch && self.checked_epoch.swap(epoch, Ordering::AcqRel) != epoch
        {
            let wanted = (self.still_wanted)(&self.shared.lock());
            self.wanted.store(wanted, Ordering::Release);
        }
        if !self.wanted.load(Ordering::Acquire) {
            return false;
        }
        if self.shared.allowance.foreground_waiting() {
            let mut permit = self.permit.lock().unwrap_or_else(PoisonError::into_inner);
            if permit.is_some() {
                // Give the permit back first, then queue behind the foreground unit.
                *permit = None;
                self.yields.fetch_add(1, Ordering::Relaxed);
                *permit = self
                    .shared
                    .allowance
                    .acquire_owned(DecodeClass::Speculative, &|| {
                        !self.stopped() && self.wanted.load(Ordering::Acquire)
                    });
                if permit.is_none() {
                    return false;
                }
            }
        }
        true
    }

    /// Takes the unit's first permit. `false` when the unit was abandoned while waiting.
    fn acquire(&self) -> bool {
        let permit = self
            .shared
            .allowance
            .acquire_owned(DecodeClass::Speculative, &|| {
                if self.stopped() {
                    return false;
                }
                let epoch = self.shared.epoch.load(Ordering::Acquire);
                if epoch != self.started_epoch
                    && self.checked_epoch.swap(epoch, Ordering::AcqRel) != epoch
                {
                    let wanted = (self.still_wanted)(&self.shared.lock());
                    self.wanted.store(wanted, Ordering::Release);
                }
                self.wanted.load(Ordering::Acquire)
            });
        let acquired = permit.is_some();
        *self.permit.lock().unwrap_or_else(PoisonError::into_inner) = permit;
        acquired
    }

    fn release(&self) {
        *self.permit.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

struct OpenSource {
    source: ProductionAlignmentSource,
    dictionary: ReferenceDictionary,
    sidecar_key_stamp: Option<rgv_hts::AlignmentSourceStamp>,
}

struct Worker {
    config: AlignmentCoverageEngineConfig,
    shared: Arc<Shared>,
    store: DepthPyramidStore,
    source: Option<Result<OpenSource, Arc<str>>>,
    /// Settings the resident tiles were built under.
    eligibility: Option<CoverageEligibility>,
    sidecar_key: Option<CoverageSidecarKey>,
    /// Tiles the sidecar is known not to hold.
    sidecar_absent: HashSet<CoverageTileId>,
    failed_units: HashMap<CoverageTileId, Arc<str>>,
    /// Units whose resident tile counts every mate independently (count-once overflowed).
    uncollapsed_units: HashSet<CoverageTileId>,
    seen_retry: u64,
    last_signatures: HashMap<SourceOwnerId, PublicationSignature>,
}

type PublicationSignature = (
    u64,
    AlignmentCoverageState,
    Vec<(CoverageTileId, u64, u64)>,
    bool,
);

/// One owner's request resolved against the open source.
struct ActiveView {
    owner: SourceOwnerId,
    generation: u64,
    view: DepthPyramidView,
}

impl Worker {
    fn new(config: AlignmentCoverageEngineConfig, shared: Arc<Shared>) -> Self {
        let store = DepthPyramidStore::new(
            config.parameters,
            config.ledger.clone(),
            config.resident_byte_limit,
        )
        .with_production_unit_bases(config.production_unit_bases);
        Self {
            config,
            shared,
            store,
            source: None,
            eligibility: None,
            sidecar_key: None,
            sidecar_absent: HashSet::new(),
            failed_units: HashMap::new(),
            uncollapsed_units: HashSet::new(),
            seen_retry: 0,
            last_signatures: HashMap::new(),
        }
    }

    fn run(mut self) {
        let mut seen_epoch = None;
        let mut worked = true;
        loop {
            let (requests, epoch, retry) = {
                let mut state = self.shared.lock();
                while !state.shutdown && !worked && Some(state.epoch) == seen_epoch {
                    if !state.idle {
                        state.idle = true;
                        self.shared.published.notify_all();
                    }
                    state = self
                        .shared
                        .work
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if state.shutdown {
                    return;
                }
                state.idle = false;
                let mut requests = state
                    .requests
                    .iter()
                    .map(|(owner, entry)| (*owner, entry.request.clone(), entry.arrival))
                    .collect::<Vec<_>>();
                // The owner that asked last is served first.
                requests.sort_by_key(|(owner, _, arrival)| (std::cmp::Reverse(*arrival), *owner));
                (requests, state.epoch, state.retry)
            };
            seen_epoch = Some(epoch);
            if retry != self.seen_retry {
                self.seen_retry = retry;
                self.failed_units.clear();
            }
            worked = self.serve(&requests);
        }
    }

    /// One round: obtain without decoding, publish, decode at most one unit, publish. Returns
    /// whether another round may find more to do without a new request.
    #[allow(clippy::too_many_lines)]
    fn serve(&mut self, requests: &[(SourceOwnerId, AlignmentCoverageRequest, u64)]) -> bool {
        self.last_signatures
            .retain(|owner, _| requests.iter().any(|(candidate, _, _)| candidate == owner));
        // Owners deferring to read evidence need no source.
        let pyramid_requests = requests
            .iter()
            .filter(|(_, request, _)| !request.exact_read_depth_ready)
            .collect::<Vec<_>>();
        for (owner, request, _) in requests {
            if request.exact_read_depth_ready {
                self.publish(
                    *owner,
                    request.generation,
                    DepthLaneAuthority::ExactReadEvidence,
                    None,
                    AlignmentCoverageState::DeferredToReadEvidence,
                    false,
                );
            }
        }
        let Some((_, newest, _)) = pyramid_requests.first() else {
            return false;
        };
        // The newest request's filter settings are the track's settings.
        if self.eligibility.as_ref() != Some(&newest.eligibility) {
            self.store.clear();
            self.sidecar_absent.clear();
            self.failed_units.clear();
            self.uncollapsed_units.clear();
            self.eligibility = Some(newest.eligibility.clone());
            self.sidecar_key = None;
        }
        let eligibility = newest.eligibility.clone();

        if self.source.is_none() {
            self.source = Some(self.open_source());
        }
        let dictionary = match self.source.as_ref() {
            Some(Ok(open)) => open.dictionary.clone(),
            Some(Err(message)) => {
                let message = Arc::clone(message);
                for (owner, request, _) in &pyramid_requests {
                    self.publish(
                        *owner,
                        request.generation,
                        DepthLaneAuthority::Pyramid,
                        None,
                        AlignmentCoverageState::Failed(Arc::clone(&message)),
                        false,
                    );
                }
                return false;
            }
            None => return false,
        };
        if self.sidecar_key.is_none()
            && let Some(Ok(open)) = &self.source
            && let Some(stamp) = open.sidecar_key_stamp
        {
            self.sidecar_key = Some(CoverageSidecarKey::new(
                stamp,
                &eligibility,
                self.config.parameters,
            ));
        }

        let mut views = Vec::new();
        for (owner, request, _) in &pyramid_requests {
            if request.eligibility != eligibility {
                // Superseded settings; the owner will ask again with the track's current ones.
                continue;
            }
            let Some(contig) = dictionary.get(request.contig) else {
                self.publish(
                    *owner,
                    request.generation,
                    DepthLaneAuthority::Pyramid,
                    None,
                    AlignmentCoverageState::Failed(Arc::from(format!(
                        "the alignment source has no contig {}",
                        request.contig.get()
                    ))),
                    false,
                );
                continue;
            };
            views.push(ActiveView {
                owner: *owner,
                generation: request.generation,
                view: DepthPyramidView {
                    contig: request.contig,
                    contig_length: contig.length(),
                    start: request.start,
                    end: request.end,
                    bases_per_pixel: request.bases_per_pixel,
                },
            });
        }

        for active in &views {
            self.obtain_without_decoding(&active.view);
        }
        self.publish_views(&views);

        // Decode one unit: the first missing one of the most recent owner that has any.
        let next = views.iter().find_map(|active| {
            let plan = self.store.plan(&active.view);
            plan.missing_visible
                .iter()
                .chain(&plan.missing_margin)
                .find(|unit| !self.failed_units.contains_key(unit))
                .map(|unit| (*unit, active.view.contig_length))
        });
        let Some((unit, contig_length)) = next else {
            self.note_store();
            return false;
        };
        self.decode_unit(unit, contig_length, &eligibility);
        for active in &views {
            self.obtain_without_decoding(&active.view);
        }
        self.publish_views(&views);
        self.note_store();
        true
    }

    fn open_source(&self) -> Result<OpenSource, Arc<str>> {
        let source = ProductionAlignmentSource::open(&self.config.path, &self.config.open_options)
            .map_err(|error| Arc::<str>::from(format!("coverage source did not open: {error}")))?;
        let dictionary = source.reference_dictionary().clone();
        let sidecar_key_stamp = source.source_stamp();
        self.shared.lock().telemetry.source_opened = true;
        Ok(OpenSource {
            source,
            dictionary,
            sidecar_key_stamp,
        })
    }

    /// Step 2: every target tile under the view and margin that can be had from the resident
    /// store, the sidecar cache, or by reducing finer tiles.
    fn obtain_without_decoding(&mut self, view: &DepthPyramidView) {
        let target = self.store.target_level(view);
        // Finer tiles that are resident or cached (from an earlier, closer view) are reduced
        // rather than decoded again, whatever level this view's own units are produced at.
        let depth = DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH;
        let (start, end) = view.with_margin();
        let parameters = self.config.parameters;
        let sidecar = self.config.sidecar.clone();
        let key = self.sidecar_key;
        let mut loaded = 0_u64;
        let mut derived = 0_u64;
        let mut stored = 0_u64;
        for index in parameters.tile_range(target, start, end, view.contig_length) {
            let id = CoverageTileId::new(view.contig, target, index);
            if self.store.contains(id) {
                continue;
            }
            let mut created = Vec::<(Arc<DepthPyramidTile>, bool)>::new();
            let absent = &mut self.sidecar_absent;
            let mut load = |id: CoverageTileId| -> Option<CoverageTile> {
                let (sidecar, key) = (sidecar.as_ref()?, key?);
                if absent.contains(&id) {
                    return None;
                }
                let tile = sidecar.load(key, id, parameters, view.contig_length);
                if tile.is_none() {
                    absent.insert(id);
                }
                tile
            };
            if let Err(error) =
                self.store
                    .obtain(id, view.contig_length, depth, &mut load, &mut created)
            {
                tracing::warn!(%error, ?id, "coverage tile could not be made resident");
            }
            for (tile, was_derived) in created {
                if was_derived {
                    derived += 1;
                    if let (Some(sidecar), Some(key)) = (&sidecar, key)
                        && sidecar.store(key, tile.tile()).is_ok()
                    {
                        stored += 1;
                        self.sidecar_absent.remove(&tile.tile().id());
                    }
                } else {
                    loaded += 1;
                }
            }
        }
        if loaded + derived + stored > 0 {
            let mut state = self.shared.lock();
            state.telemetry.sidecar_tiles_loaded += loaded;
            state.telemetry.tiles_derived += derived;
            state.telemetry.sidecar_tiles_stored += stored;
        }
    }

    /// Step 4: streams one unit from the source into a tile.
    #[allow(clippy::too_many_lines)]
    fn decode_unit(
        &mut self,
        unit: CoverageTileId,
        contig_length: u64,
        eligibility: &CoverageEligibility,
    ) {
        let parameters = self.config.parameters;
        let probe_eligibility = eligibility.clone();
        let started_epoch = self.shared.epoch.load(Ordering::Acquire);
        let unit_span = parameters.tile_span(unit, contig_length);
        let probe = Arc::new(UnitProbe {
            shared: Arc::clone(&self.shared),
            started_epoch,
            checked_epoch: AtomicU64::new(started_epoch),
            wanted: AtomicBool::new(true),
            still_wanted: Box::new(move |state| {
                let Some((unit_start, unit_end)) = unit_span else {
                    return false;
                };
                state.requests.values().any(|entry| {
                    let request = &entry.request;
                    if request.exact_read_depth_ready
                        || request.contig != unit.contig
                        || request.eligibility != probe_eligibility
                    {
                        return false;
                    }
                    let view = DepthPyramidView {
                        contig: request.contig,
                        contig_length,
                        start: request.start,
                        end: request.end,
                        bases_per_pixel: request.bases_per_pixel,
                    };
                    let (start, end) = view.with_margin();
                    if unit_start >= end || unit_end <= start {
                        return false;
                    }
                    // Only a request drawn from (or derived from) this level uses the unit.
                    let target = parameters
                        .level_for_bases_per_pixel(view.bases_per_pixel)
                        .min(parameters.root_level(contig_length));
                    unit.level <= target
                        && target - unit.level <= DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH
                })
            }),
            permit: Mutex::new(None),
            yields: AtomicU64::new(0),
        });
        if !probe.acquire() {
            self.shared.lock().telemetry.units_cancelled += 1;
            return;
        }
        let cancellation: RemoteCancellation = {
            let probe = Arc::clone(&probe);
            Arc::new(move || probe.keep_going())
        };
        let request = CoverageTileRequest {
            parameters,
            contig: unit.contig,
            level: unit.level,
            tiles: unit.index..unit.index.saturating_add(1),
            eligibility: eligibility.clone(),
        };
        let budget = self.config.remote_unit_transfer_budget;
        let outcome = match self.source.as_mut() {
            Some(Ok(open)) => open
                .source
                .coverage_tiles(&request, Some(budget), &cancellation),
            _ => return,
        };
        probe.release();
        let yields = probe.yields.load(Ordering::Relaxed);
        let mut failure = None;
        let mut decoded = None;
        match outcome {
            Ok(CoverageTileOutcome::Complete(production)) => {
                // An overflowed count-once window is not a refusal: the source has rebuilt
                // the unit with every mate counted independently and says so. Only a
                // production that is neither exact nor flagged is unusable.
                if production.overlap_collapse_complete || production.mate_overlap_not_collapsed {
                    decoded = Some(production);
                } else {
                    failure = Some(Arc::<str>::from(
                        "the count-once mate window overflowed; exact coverage is unavailable \
                         for this region",
                    ));
                }
            }
            Ok(CoverageTileOutcome::Cancelled) => {}
            Err(error) => {
                failure = Some(Arc::from(format!("coverage could not be read: {error}")));
            }
        }
        let cancelled = decoded.is_none() && failure.is_none();
        let mut stored = 0_u64;
        let mut records = 0_u64;
        let mut working_bytes = 0_usize;
        let mut uncollapsed = false;
        if let Some(production) = decoded {
            records = production.records_seen;
            working_bytes = production.telemetry.peak_depth_state_bytes();
            uncollapsed = production.mate_overlap_not_collapsed;
            if uncollapsed {
                self.uncollapsed_units.insert(unit);
            } else {
                self.uncollapsed_units.remove(&unit);
            }
            for tile in production.tiles {
                // The sidecar is keyed by the requested (count-once) settings and has no
                // place for the flag, so a degraded tile is never persisted under that key.
                if !uncollapsed
                    && let (Some(sidecar), Some(key)) = (&self.config.sidecar, self.sidecar_key)
                {
                    if sidecar.store(key, &tile).is_ok() {
                        stored += 1;
                    }
                    self.sidecar_absent.remove(&tile.id());
                }
                if let Err(error) = self.store.insert(tile) {
                    failure = Some(Arc::from(format!(
                        "coverage tile could not be retained: {error}"
                    )));
                }
            }
        }
        if let Some(message) = &failure {
            self.failed_units.insert(unit, Arc::clone(message));
        }
        let mut state = self.shared.lock();
        let telemetry = &mut state.telemetry;
        telemetry.foreground_yields += yields;
        telemetry.sidecar_tiles_stored += stored;
        telemetry.records_streamed += records;
        telemetry.peak_unit_working_bytes = telemetry.peak_unit_working_bytes.max(working_bytes);
        if cancelled {
            telemetry.units_cancelled += 1;
        } else if failure.is_some() {
            telemetry.units_failed += 1;
        } else {
            telemetry.units_decoded += 1;
            telemetry.units_mate_overlap_not_collapsed += u64::from(uncollapsed);
        }
    }

    fn publish_views(&mut self, views: &[ActiveView]) {
        for active in views {
            let lane = self.store.lane(&active.view);
            let plan = self.store.plan(&active.view);
            let visible_failure = plan
                .missing_visible
                .iter()
                .find_map(|unit| self.failed_units.get(unit))
                .cloned();
            let state = if let Some(message) = visible_failure {
                AlignmentCoverageState::Failed(message)
            } else if lane.is_exact_at_target() {
                AlignmentCoverageState::Complete {
                    missing_margin_units: plan.missing_margin.len(),
                }
            } else {
                AlignmentCoverageState::Refining {
                    missing_visible_units: plan.missing_visible.len(),
                    missing_margin_units: plan.missing_margin.len(),
                }
            };
            // Any degraded unit under the visible span, at whatever level it was produced:
            // the tiles derived from it carry its counts.
            let (visible_start, visible_end) = active.view.visible();
            let parameters = self.config.parameters;
            let uncollapsed = self.uncollapsed_units.iter().any(|unit| {
                unit.contig == active.view.contig
                    && parameters
                        .tile_span(*unit, active.view.contig_length)
                        .is_some_and(|(start, end)| start < visible_end && end > visible_start)
            });
            self.publish(
                active.owner,
                active.generation,
                DepthLaneAuthority::Pyramid,
                Some(lane),
                state,
                uncollapsed,
            );
        }
    }

    /// Publishes for `owner` unless nothing changed or the owner has asked again.
    fn publish(
        &mut self,
        owner: SourceOwnerId,
        generation: u64,
        authority: DepthLaneAuthority,
        lane: Option<DepthPyramidLane>,
        state: AlignmentCoverageState,
        mate_overlap_not_collapsed: bool,
    ) {
        let signature: PublicationSignature = (
            generation,
            state.clone(),
            lane.as_ref()
                .map(DepthPyramidLane::signature)
                .unwrap_or_default(),
            mate_overlap_not_collapsed,
        );
        if self.last_signatures.get(&owner) == Some(&signature) {
            return;
        }
        let mut shared = self.shared.lock();
        // Latest wins: a publication for a request the owner has replaced is dropped.
        if shared
            .requests
            .get(&owner)
            .is_none_or(|current| current.request.generation != generation)
        {
            shared.telemetry.superseded_publications += 1;
            return;
        }
        shared.sequence += 1;
        shared.telemetry.publications += 1;
        let sequence = shared.sequence;
        shared.publications.insert(
            owner,
            Arc::new(AlignmentCoveragePublication {
                owner,
                generation,
                sequence,
                authority,
                lane: lane.map(Arc::new),
                state,
                mate_overlap_not_collapsed,
            }),
        );
        drop(shared);
        self.last_signatures.insert(owner, signature);
        self.shared.published.notify_all();
        crate::mailbox::wake_ui();
    }

    fn note_store(&mut self) {
        self.store.trim();
        let mut state = self.shared.lock();
        state.telemetry.resident_tiles = self.store.len();
        state.telemetry.resident_bytes = self.store.resident_bytes();
    }
}

#[cfg(test)]
mod tests;
