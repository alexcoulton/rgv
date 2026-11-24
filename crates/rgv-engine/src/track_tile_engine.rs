//! Tile engine for auxiliary tracks: the scheduling, caching and cancellation half of the
//! public track contract.
//!
//! * **Tiles, not viewports.** A view (panel, track, camera) is turned into the tiles of one
//!   pyramid level that intersect it, plus a margin. Each tile is read once, laid out once in
//!   world space on a helper thread, and cached per track under
//!   `(contig, tile, settings digest)` with a byte budget and LRU eviction. Panning inside
//!   cached tiles does no work at all.
//! * **Per-track isolation.** A fixed pool of helpers serves every track; a track has at most
//!   one operation in flight (its source is `&mut`), so one slow or failing track occupies one
//!   helper and never delays another track. Opening a source is such an operation too. The
//!   number of threads does not grow with the number of tracks.
//! * **Latest wins, with real cancellation.** A new view for a (panel, track) replaces the old
//!   one. A tile read that no current view wants any more has its cancellation raised, which
//!   aborts remote transfers mid-request; a tile that is still wanted keeps loading and is
//!   delivered under the new generation.
//! * **Priorities.** Helpers take, in order: sources to open, name-search and feature-step
//!   parts, visible tiles, margin tiles; round-robin over tracks within a class. The order
//!   holds across tracks, not only when a helper picks its next job: margin reads may occupy
//!   only part of the pool ([`margin_read_limit`]), and when a visible tile has no free
//!   helper, margin reads already in flight are cancelled to make room (they are read again
//!   later). A track whose own in-flight read is a margin tile gives it up for a visible one.
//! * **One reader per track.** A track the legacy [`crate::TrackEngine`] already has open can
//!   be served from that engine's reader instead of a second one:
//!   [`crate::TrackEngine::try_share_tile_sources`].
//!
//! * **Records on demand.** A tile carries what is drawn. The complete record behind a
//!   [`TrackRecordId`] (feature type, attributes, every block, the coding interval) is resolved
//!   by [`TrackTileEngine::lookup_record`]: from the records the tile's source handed over
//!   with the tile when it kept them, else by one bounded, cancellable re-read of the
//!   record's own interval on a helper.
//! * **Row cap.** [`TrackTileSettings::maximum_feature_rows`] of the view bounds the packed
//!   feature rows a scene draws; the scene states what the cap hid.
//!
//! The engine never blocks its caller: every method takes one short lock and returns
//! ([`TrackTileEngine::lookup_record_blocking`], which says so in its name, excepted).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use rgv_core::TrackId;
use rgv_hts::{HttpRangeConfig, MAX_FEATURE_SEARCH_RESULTS};
use rgv_model::{
    FeatureRecord, TrackCancellation, TrackRecord, TrackRecordId, TrackRecordLookup,
    TrackRecordQuery, TrackTile, TrackTileError, TrackTileKey, TrackTileRequest, TrackTileScene,
    TrackTileSettings,
};

use crate::{
    EngineQueueError, TrackSearchHit, TrackSearchResults, TrackSourceDiagnostics, TrackSpec,
    TrackSpecKind, TrackTileLayoutRegistry, TrackTileSearch, TrackTileSearchHits, TrackTileSource,
    mailbox::{
        CoalescingReceiver, CoalescingSender, bounded_coalescing_to_ui, panic_message, reap_worker,
    },
    track_engine::{TdfSourceCache, open_tile_source},
};

/// Resolved record lookups one track remembers (least recently resolved are forgotten).
const RESOLVED_RECORD_LOOKUPS: usize = 64;

/// Default byte budget of one track's tile cache.
pub const DEFAULT_TRACK_TILE_CACHE_BYTES: usize = 64 << 20;
/// Most helper threads [`TrackTileEngine::spawn`] starts, whatever the number of tracks.
pub const MAXIMUM_TRACK_TILE_HELPERS: usize = 8;
/// Most helper threads any engine starts ([`TrackTileEngine::spawn_with_helper_limit`]).
pub const MAXIMUM_CONFIGURABLE_TRACK_TILE_HELPERS: usize = 64;

/// How many of `helpers` helpers may read margin tiles at the same time. At least one helper
/// in four (and always one, when there is more than one) is kept for visible tiles, searches
/// and opens, so prefetching can never occupy the whole pool.
#[must_use]
pub const fn margin_read_limit(helpers: usize) -> usize {
    if helpers <= 1 {
        return 1;
    }
    let reserved = if helpers / 4 > 1 { helpers / 4 } else { 1 };
    helpers - reserved
}

#[derive(Clone)]
pub struct TrackTileEngineConfig {
    /// Helper threads; clamped to `1..=MAXIMUM_TRACK_TILE_HELPERS` by
    /// [`TrackTileEngine::spawn`], or to the limit given to
    /// [`TrackTileEngine::spawn_with_helper_limit`].
    pub helpers: usize,
    pub cache_bytes_per_track: usize,
    /// Tiles prefetched on each side of the visible ones, after every visible tile of every
    /// track.
    pub margin_tiles: u64,
    /// Capacity of the event queue's non-discardable part.
    pub event_capacity: usize,
    /// Authentication, timeouts, retries and cache geometry of remote sources.
    pub remote: HttpRangeConfig,
    pub layouts: TrackTileLayoutRegistry,
}

impl Default for TrackTileEngineConfig {
    fn default() -> Self {
        Self {
            helpers: MAXIMUM_TRACK_TILE_HELPERS
                .min(thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get)),
            cache_bytes_per_track: DEFAULT_TRACK_TILE_CACHE_BYTES,
            margin_tiles: 1,
            event_capacity: 256,
            remote: HttpRangeConfig::default(),
            layouts: TrackTileLayoutRegistry::builtin(),
        }
    }
}

/// A tile and its world-space layout, immutable and shared between the cache, the events and
/// every panel that shows it.
#[derive(Debug)]
pub struct LaidOutTrackTile {
    pub tile: TrackTile,
    pub scene: TrackTileScene,
    /// Digest of the settings the tile was built with; with the track id and `tile.key` it
    /// identifies the tile's GPU page.
    pub settings_digest: u64,
}

impl LaidOutTrackTile {
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.tile
            .resident_bytes()
            .saturating_add(self.scene.resident_bytes())
    }

    /// The contig of the tile, as its source spells it. With the track id, `tile.key` and
    /// `settings_digest` it completes the identity of the tile and of its GPU page
    /// (`rgv_render::TrackTilePageKey::for_contig`): a tile key alone names no contig.
    #[must_use]
    pub fn contig(&self) -> &str {
        &self.tile.contig
    }
}

/// What one panel wants to see of one track.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackTileView {
    /// Stable owner of the view (the panel id).
    pub panel: u64,
    pub track: TrackId,
    /// Workspace contig name; the track's contig aliases are applied by the engine.
    pub contig: String,
    /// Visible genomic interval, zero-based half-open, in bases (fractional while panning).
    pub start: f64,
    pub end: f64,
    /// Camera resolution; selects the pyramid level.
    pub bases_per_point: f64,
    /// Tiles past the end of the contig are not requested when this is known.
    pub contig_length: Option<u64>,
    pub settings: TrackTileSettings,
}

/// Current state of a view, read whenever the caller wants to draw.
#[derive(Clone, Debug, Default)]
pub struct TrackTileViewSnapshot {
    pub generation: u64,
    pub level: u8,
    /// Visible tiles that are ready, left to right.
    pub tiles: Vec<Arc<LaidOutTrackTile>>,
    /// Visible tiles still loading.
    pub missing: Vec<TrackTileKey>,
    /// Visible tiles whose read failed, with the message.
    pub failed: Vec<(TrackTileKey, String)>,
    /// Cached tiles of other levels (same contig and settings) that intersect the view; drawn
    /// beneath `tiles` they fill the gaps while a zoom's own level loads.
    pub fallback: Vec<Arc<LaidOutTrackTile>>,
    /// Contig of the view as the workspace spells it ([`TrackTileView::contig`]); empty for
    /// a default snapshot.
    pub contig: String,
    /// The same contig as the track's source spells it (after the track's aliases): the
    /// contig of every tile in `tiles`, `missing`, `failed` and `fallback`.
    pub source_contig: String,
    /// Digest of the view's settings; the `settings_digest` of its tiles.
    pub settings_digest: u64,
}

impl TrackTileViewSnapshot {
    /// Contig the view was requested for, as the workspace spells it.
    #[must_use]
    pub fn contig(&self) -> &str {
        &self.contig
    }

    /// Contig of the view's tiles, as the track's source spells it.
    #[must_use]
    pub fn source_contig(&self) -> &str {
        &self.source_contig
    }

    /// What the view's row cap hid over the ready tiles: the deepest hidden row count and the
    /// number of distinct hidden features. See [`TrackTileSettings::maximum_feature_rows`].
    #[must_use]
    pub fn overflow(&self) -> rgv_model::TrackSceneOverflow {
        TrackTileScene::lane_overflow(self.tiles.iter().map(|tile| &tile.scene))
    }

    /// Every visible tile is ready or has failed.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.missing.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackTileEngineEvent {
    TrackOpened {
        track: TrackId,
    },
    TrackOpenFailed {
        track: TrackId,
        message: String,
    },
    /// A tile of the view became available; `TrackTileEngine::view` returns it.
    TileReady {
        panel: u64,
        track: TrackId,
        generation: u64,
        key: TrackTileKey,
    },
    TileFailed {
        panel: u64,
        track: TrackId,
        generation: u64,
        key: TrackTileKey,
        message: String,
    },
    /// Every visible tile of the view is ready or failed.
    ViewSettled {
        panel: u64,
        track: TrackId,
        generation: u64,
    },
    SearchCompleted {
        generation: u64,
        results: TrackSearchResults,
    },
    AdjacentFeatureCompleted {
        generation: u64,
        hit: Option<Box<TrackSearchHit>>,
    },
}

/// Counters of one track's tile cache and scheduling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackTileCacheStats {
    pub tiles: usize,
    pub bytes: usize,
    pub budget_bytes: usize,
    /// Visible tiles a view request found already cached.
    pub hits: u64,
    /// Visible tiles a view request had to schedule.
    pub misses: u64,
    pub evictions: u64,
    /// Tile reads the source completed.
    pub completed: u64,
    /// Tile reads abandoned because no view wanted them any more.
    pub cancelled: u64,
    pub failed: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    /// Contig as the source spells it.
    contig: Arc<str>,
    tile: TrackTileKey,
    digest: u64,
}

/// Complete records a source handed over with a tile, by identity.
type RetainedRecords = HashMap<TrackRecordId, Arc<TrackRecord>>;

struct CacheEntry {
    tile: Arc<LaidOutTrackTile>,
    /// Empty when the source kept nothing for this tile.
    records: RetainedRecords,
    bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct TileCache {
    entries: HashMap<CacheKey, CacheEntry>,
    bytes: usize,
    tick: u64,
}

impl TileCache {
    fn get(&mut self, key: &CacheKey) -> Option<Arc<LaidOutTrackTile>> {
        self.tick += 1;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.tick;
        Some(Arc::clone(&entry.tile))
    }

    /// Inserts `tile` and evicts least-recently-used entries no view wants until the cache
    /// fits `budget`. Returns the number evicted. Tiles a view still wants are never evicted,
    /// so a budget smaller than one screen of tiles is exceeded rather than thrashed.
    fn insert(
        &mut self,
        key: CacheKey,
        tile: Arc<LaidOutTrackTile>,
        records: RetainedRecords,
        budget: usize,
        wanted: &dyn Fn(&CacheKey) -> bool,
    ) -> u64 {
        self.tick += 1;
        let bytes = tile.resident_bytes().saturating_add(
            records
                .values()
                .map(|record| record.resident_bytes() + size_of::<TrackRecordId>())
                .sum(),
        );
        if let Some(previous) = self.entries.insert(
            key,
            CacheEntry {
                tile,
                records,
                bytes,
                last_used: self.tick,
            },
        ) {
            self.bytes -= previous.bytes;
        }
        self.bytes += bytes;
        let mut evicted = 0;
        while self.bytes > budget {
            let Some(victim) = self
                .entries
                .iter()
                .filter(|(key, _)| !wanted(key))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&victim) {
                self.bytes -= entry.bytes;
                evicted += 1;
            }
        }
        evicted
    }
}

struct View {
    generation: u64,
    /// Contig as the workspace spells it.
    contig: Arc<str>,
    source_contig: Arc<str>,
    level: u8,
    digest: u64,
    settings: TrackTileSettings,
    start: u64,
    end: u64,
    visible: Vec<TrackTileKey>,
    /// Nearest first.
    margin: Vec<TrackTileKey>,
    failed: BTreeMap<TrackTileKey, String>,
    settled: bool,
}

impl View {
    fn cache_key(&self, tile: TrackTileKey) -> CacheKey {
        CacheKey {
            contig: Arc::clone(&self.source_contig),
            tile,
            digest: self.digest,
        }
    }

    fn wants(&self, key: &CacheKey) -> bool {
        key.digest == self.digest
            && key.contig == self.source_contig
            && (self.visible.contains(&key.tile) || self.margin.contains(&key.tile))
    }

    /// Whether `key` is one of the tiles the view shows (not merely prefetches).
    fn shows(&self, key: &CacheKey) -> bool {
        key.digest == self.digest
            && key.contig == self.source_contig
            && self.visible.contains(&key.tile)
    }
}

enum SourceSlot {
    Unopened(TrackSpec),
    Idle(Box<dyn TrackTileSource>),
    /// Lent to a helper, or being opened by one, or awaited from the legacy track engine.
    Busy,
    Failed,
}

struct InFlight {
    cancel: Arc<AtomicBool>,
    /// The tile being read, when the operation is a tile read.
    tile: Option<CacheKey>,
}

struct TrackState {
    /// Distinguishes this registration from an earlier one under the same id: an operation
    /// started for a replaced registration must not hand its source to the new one.
    epoch: u64,
    name: String,
    aliases: BTreeMap<String, String>,
    source: SourceSlot,
    in_flight: Option<InFlight>,
    views: BTreeMap<u64, View>,
    cache: TileCache,
    stats: TrackTileCacheStats,
    diagnostics: Option<TrackSourceDiagnostics>,
    /// The record lookup waiting for or occupying this track's source; the newest wins.
    lookup: Option<PendingLookup>,
    /// Finished lookups, oldest first, at most [`RESOLVED_RECORD_LOOKUPS`].
    resolved: VecDeque<(LookupKey, TrackRecordLookup)>,
    /// Why the source is [`SourceSlot::Failed`], for views requested afterwards.
    failure: Option<String>,
}

/// Source contig and identity of a looked-up record.
type LookupKey = (Arc<str>, TrackRecordId);

struct PendingLookup {
    key: LookupKey,
    query: TrackRecordQuery,
    cancel: Arc<AtomicBool>,
    /// A helper is running it.
    started: bool,
}

impl TrackState {
    fn new(
        epoch: u64,
        name: String,
        aliases: BTreeMap<String, String>,
        source: SourceSlot,
    ) -> Self {
        Self {
            epoch,
            name,
            aliases,
            source,
            in_flight: None,
            views: BTreeMap::new(),
            cache: TileCache::default(),
            stats: TrackTileCacheStats::default(),
            diagnostics: None,
            lookup: None,
            resolved: VecDeque::new(),
            failure: None,
        }
    }

    /// Records the outcome of a lookup, forgetting the oldest beyond the bound.
    fn resolve(&mut self, key: LookupKey, outcome: TrackRecordLookup) {
        self.resolved.retain(|(resolved, _)| *resolved != key);
        self.resolved.push_back((key, outcome));
        while self.resolved.len() > RESOLVED_RECORD_LOOKUPS {
            self.resolved.pop_front();
        }
    }

    fn wanted(&self, key: &CacheKey) -> bool {
        self.views.values().any(|view| view.wants(key))
    }

    /// The tile of an in-flight read that no view shows: a margin read, or one whose view
    /// has moved on.
    fn in_flight_margin(&self) -> Option<&InFlight> {
        let in_flight = self.in_flight.as_ref()?;
        let tile = in_flight.tile.as_ref()?;
        (!self.views.values().any(|view| view.shows(tile))).then_some(in_flight)
    }
}

enum FanOutKind {
    Search(TrackTileSearch),
    Adjacent {
        contig: String,
        position: u64,
        forward: bool,
    },
}

/// One name search or feature step, spread over every track.
struct FanOut {
    generation: u64,
    kind: Arc<FanOutKind>,
    /// Tracks that have not started their part.
    remaining: BTreeSet<TrackId>,
    running: usize,
    cancel: Arc<AtomicBool>,
    hits: Vec<TrackSearchHit>,
    index_truncated: bool,
    scan_truncated: bool,
}

#[derive(Default)]
struct State {
    shutdown: bool,
    tracks: BTreeMap<TrackId, TrackState>,
    /// Track served last, for round-robin fairness.
    cursor: Option<TrackId>,
    next_generation: u64,
    next_epoch: u64,
    /// Helper threads started.
    helpers: usize,
    /// Helpers waiting for work right now.
    waiting: usize,
    search: Option<FanOut>,
    adjacent: Option<FanOut>,
}

struct Shared {
    state: Mutex<State>,
    work: Condvar,
    output: CoalescingSender<TrackTileEngineEvent>,
    layouts: TrackTileLayoutRegistry,
    remote: HttpRangeConfig,
    /// TDF channels of one file share a reader; held only while a TDF source is opened.
    tdf_sources: Mutex<TdfSourceCache>,
    cache_bytes_per_track: usize,
    margin_tiles: u64,
    /// Signalled, with the state lock, whenever a record lookup finishes.
    lookups: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Publishes events. Never called with the state locked: a non-discardable event may wait
    /// for the receiver to drain.
    fn publish(&self, events: Vec<TrackTileEngineEvent>) {
        for event in events {
            let result = match &event {
                TrackTileEngineEvent::TileReady {
                    panel, track, key, ..
                } => self.output.send_latest_system_for(
                    // Namespace 0 is the settle notifications'.
                    mix(*panel, track.get()).clamp(1, u64::MAX - 1),
                    mix(u64::from(key.level), key.index),
                    event,
                ),
                TrackTileEngineEvent::ViewSettled { panel, track, .. } => self
                    .output
                    .send_latest_system_for(0, mix(*panel, track.get()), event),
                TrackTileEngineEvent::SearchCompleted { .. } => {
                    self.output.send_latest_for(u64::MAX, event)
                }
                TrackTileEngineEvent::AdjacentFeatureCompleted { .. } => {
                    self.output.send_latest_for(u64::MAX - 1, event)
                }
                TrackTileEngineEvent::TrackOpened { .. }
                | TrackTileEngineEvent::TrackOpenFailed { .. }
                | TrackTileEngineEvent::TileFailed { .. } => self.output.send_control(event),
            };
            if result.is_err() {
                tracing::debug!("track tile event receiver closed");
                return;
            }
        }
    }
}

/// Mixes two words into a coalescing key (splitmix64 finaliser).
fn mix(left: u64, right: u64) -> u64 {
    let mut value = left
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(right)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// The tile engine handle. Dropping it cancels everything in flight and returns at once.
pub struct TrackTileEngine {
    shared: Arc<Shared>,
    output: CoalescingReceiver<TrackTileEngineEvent>,
    helpers: Vec<JoinHandle<()>>,
}

impl TrackTileEngine {
    #[must_use]
    pub fn spawn(config: TrackTileEngineConfig) -> Self {
        Self::spawn_with_helper_limit(config, MAXIMUM_TRACK_TILE_HELPERS)
    }

    /// As [`Self::spawn`], with `config.helpers` clamped to `1..=maximum_helpers` instead of
    /// the default limit (itself at most [`MAXIMUM_CONFIGURABLE_TRACK_TILE_HELPERS`]).
    ///
    /// A track occupies a helper for as long as its source takes to answer, so a workspace
    /// with more simultaneously slow tracks (remote files on a slow link) than helpers makes
    /// the remaining tracks wait until one answers or is cancelled. A larger pool is the
    /// remedy; its threads sleep while there is nothing to read.
    #[must_use]
    pub fn spawn_with_helper_limit(config: TrackTileEngineConfig, maximum_helpers: usize) -> Self {
        let maximum_helpers = maximum_helpers.clamp(1, MAXIMUM_CONFIGURABLE_TRACK_TILE_HELPERS);
        let (output_tx, output) = bounded_coalescing_to_ui(config.event_capacity.max(1));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            work: Condvar::new(),
            output: output_tx,
            layouts: config.layouts,
            remote: config.remote,
            tdf_sources: Mutex::new(HashMap::new()),
            cache_bytes_per_track: config.cache_bytes_per_track,
            margin_tiles: config.margin_tiles,
            lookups: Condvar::new(),
        });
        let helpers = (0..config.helpers.clamp(1, maximum_helpers))
            .filter_map(|ordinal| {
                let shared = Arc::clone(&shared);
                thread::Builder::new()
                    .name(format!("rgv-track-tile-{ordinal}"))
                    .spawn(move || helper_loop(&shared))
                    .inspect_err(
                        |error| tracing::warn!(%error, "could not start a track tile helper"),
                    )
                    .ok()
            })
            .collect::<Vec<_>>();
        shared.lock().helpers = helpers.len();
        Self {
            shared,
            output,
            helpers,
        }
    }

    /// Helper threads this engine runs.
    #[must_use]
    pub fn helper_count(&self) -> usize {
        self.helpers.len()
    }

    /// The handle through which the legacy track engine registers the sources it shares.
    pub(crate) fn registrar(&self) -> TrackTileSourceRegistrar {
        TrackTileSourceRegistrar {
            shared: Arc::downgrade(&self.shared),
        }
    }

    /// Registers a file-backed track. It is opened on a helper; `TrackOpened` or
    /// `TrackOpenFailed` follows. Views may be requested immediately.
    pub fn add_track(&self, specification: TrackSpec, aliases: BTreeMap<String, String>) {
        let mut state = self.shared.lock();
        register_track(
            &mut state,
            specification.id,
            specification.name.clone(),
            aliases,
            SourceSlot::Unopened(specification),
        );
        drop(state);
        self.shared.work.notify_all();
    }

    /// Registers a track backed by any [`TrackTileSource`]: the plugin entry point.
    pub fn add_source(&self, id: TrackId, name: String, source: Box<dyn TrackTileSource>) {
        let mut state = self.shared.lock();
        register_track(
            &mut state,
            id,
            name,
            BTreeMap::new(),
            SourceSlot::Idle(source),
        );
        drop(state);
        self.shared.work.notify_all();
    }

    /// Workspace-to-source contig names of a track (`1 -> chr1`). Cached tiles are keyed by
    /// the source spelling, so they stay valid.
    pub fn set_contig_aliases(&self, id: TrackId, aliases: BTreeMap<String, String>) {
        if let Some(track) = self.shared.lock().tracks.get_mut(&id) {
            track.aliases = aliases;
        }
    }

    /// Forgets tracks, their caches and views, and cancels what they have in flight.
    pub fn remove_tracks(&self, ids: &[TrackId]) {
        let mut state = self.shared.lock();
        for id in ids {
            if let Some(track) = state.tracks.remove(id) {
                if let Some(in_flight) = &track.in_flight {
                    in_flight.cancel.store(true, Ordering::Release);
                }
                if let Some(lookup) = &track.lookup {
                    lookup.cancel.store(true, Ordering::Release);
                }
            }
        }
        drop(state);
        self.shared.lookups.notify_all();
        self.shared
            .tdf_sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, source| Arc::strong_count(source) > 1);
    }

    /// Replaces the view of `(view.panel, view.track)` and returns its generation. Tiles the
    /// new view does not want stop loading; tiles it shares with the old view keep loading.
    /// `None` when the track is unknown.
    pub fn request_view(&self, view: TrackTileView) -> Option<u64> {
        let mut events = Vec::new();
        let mut state = self.shared.lock();
        state.next_generation += 1;
        let generation = state.next_generation;
        let margin_tiles = self.shared.margin_tiles;
        let track = state.tracks.get_mut(&view.track)?;
        let source_contig: Arc<str> = Arc::from(
            track
                .aliases
                .get(&view.contig)
                .map_or(view.contig.as_str(), String::as_str),
        );
        let level = TrackTileKey::level_for_bases_per_point(view.bases_per_point);
        let limit = view.contig_length.unwrap_or(u64::MAX);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (start, end) = (
            (view.start.max(0.0).floor() as u64).min(limit),
            (view.end.max(0.0).ceil() as u64).min(limit),
        );
        let visible = TrackTileKey::covering(level, start, end).collect::<Vec<_>>();
        let mut margin = Vec::new();
        if let (Some(first), Some(last)) = (visible.first(), visible.last()) {
            for distance in 1..=margin_tiles {
                let right = TrackTileKey {
                    level,
                    index: last.index + distance,
                };
                if right.start() < limit {
                    margin.push(right);
                }
                if let Some(index) = first.index.checked_sub(distance) {
                    margin.push(TrackTileKey { level, index });
                }
            }
        }
        let new_view = View {
            generation,
            contig: Arc::from(view.contig.as_str()),
            source_contig,
            level,
            digest: view.settings.digest(),
            settings: view.settings,
            start,
            end,
            visible,
            margin,
            failed: BTreeMap::new(),
            settled: false,
        };
        for tile in &new_view.visible {
            if track.cache.get(&new_view.cache_key(*tile)).is_some() {
                track.stats.hits += 1;
            } else {
                track.stats.misses += 1;
            }
        }
        let mut new_view = new_view;
        // A track that failed to open before this view was requested will never read: the
        // view's uncached tiles fail now, with the reason, instead of loading for ever.
        if matches!(track.source, SourceSlot::Failed)
            && let Some(message) = &track.failure
        {
            for tile in new_view.visible.clone() {
                if track.cache.entries.contains_key(&new_view.cache_key(tile)) {
                    continue;
                }
                new_view.failed.insert(tile, message.clone());
                events.push(TrackTileEngineEvent::TileFailed {
                    panel: view.panel,
                    track: view.track,
                    generation,
                    key: tile,
                    message: message.clone(),
                });
            }
        }
        track.views.insert(view.panel, new_view);
        // A read nobody wants any more stops now, not when its transfer happens to finish.
        if let Some(in_flight) = &track.in_flight
            && let Some(tile) = &in_flight.tile
            && !track.wanted(tile)
        {
            in_flight.cancel.store(true, Ordering::Release);
        }
        settle_views(view.track, track, &mut events);
        // Visible work that has no free helper takes one from the margin reads.
        preempt_margin_reads(&mut state);
        drop(state);
        self.shared.work.notify_all();
        self.shared.publish(events);
        Some(generation)
    }

    /// Stops serving `(panel, track)`; its tiles stay cached until evicted.
    pub fn drop_view(&self, panel: u64, track: TrackId) {
        let mut state = self.shared.lock();
        if let Some(track) = state.tracks.get_mut(&track) {
            track.views.remove(&panel);
            if let Some(in_flight) = &track.in_flight
                && let Some(tile) = &in_flight.tile
                && !track.wanted(tile)
            {
                in_flight.cancel.store(true, Ordering::Release);
            }
        }
    }

    /// What is ready for `(panel, track)` right now. `None` when there is no such view.
    #[must_use]
    pub fn view(&self, panel: u64, track: TrackId) -> Option<TrackTileViewSnapshot> {
        let mut state = self.shared.lock();
        let track = state.tracks.get_mut(&track)?;
        let view = track.views.get(&panel)?;
        let mut snapshot = TrackTileViewSnapshot {
            generation: view.generation,
            level: view.level,
            contig: view.contig.to_string(),
            source_contig: view.source_contig.to_string(),
            settings_digest: view.digest,
            ..TrackTileViewSnapshot::default()
        };
        for tile in &view.visible {
            if let Some(ready) = track.cache.get(&view.cache_key(*tile)) {
                snapshot.tiles.push(ready);
            } else if let Some(message) = view.failed.get(tile) {
                snapshot.failed.push((*tile, message.clone()));
            } else {
                snapshot.missing.push(*tile);
            }
        }
        if !snapshot.missing.is_empty() {
            snapshot.fallback = track
                .cache
                .entries
                .iter()
                .filter(|(key, _)| {
                    key.digest == view.digest
                        && key.contig == view.source_contig
                        && key.tile.level != view.level
                        && key.tile.start() < view.end
                        && key.tile.end() > view.start
                })
                .map(|(_, entry)| Arc::clone(&entry.tile))
                .collect();
            // Coarsest first, so finer tiles are drawn over coarser ones.
            snapshot
                .fallback
                .sort_by_key(|tile| (std::cmp::Reverse(tile.tile.key.level), tile.tile.key.index));
        }
        Some(snapshot)
    }

    /// The complete record behind `id`, an identity a tile of `track` on `contig` (workspace
    /// spelling; the track's aliases are applied) showed: the same [`FeatureRecord`] or
    /// variant record the legacy query returns, with its type, attributes, every block and
    /// the coding interval.
    ///
    /// * [`TrackRecordLookup::Ready`] at once when a cached tile retained the record (the
    ///   built-in feature sources hand over each tile's records up to a byte bound) or an
    ///   earlier lookup resolved it.
    /// * Otherwise one read of the record's own interval is scheduled on a helper, ahead of
    ///   tile reads, and [`TrackRecordLookup::Pending`] is returned; call again to collect
    ///   the outcome. The UI waker ([`crate::set_ui_waker`]) is invoked when it finishes.
    /// * [`TrackRecordLookup::NotFound`] when no cached tile shows `id` (its interval is then
    ///   unknown and nothing is read), or the source has no such record.
    ///
    /// A track runs one lookup at a time and the newest wins: asking for another record
    /// cancels the pending one, including its remote transfer, as do
    /// [`Self::cancel_record_lookup`], removing the track and dropping the engine. A failed
    /// or not-found outcome is reported once and then forgotten, so asking again retries.
    pub fn lookup_record(
        &self,
        track: TrackId,
        contig: &str,
        id: TrackRecordId,
    ) -> TrackRecordLookup {
        let mut state = self.shared.lock();
        let Some(track) = state.tracks.get_mut(&track) else {
            return TrackRecordLookup::NotFound;
        };
        let source_contig: Arc<str> =
            Arc::from(track.aliases.get(contig).map_or(contig, String::as_str));
        // 1. A cached tile that kept the record; otherwise the interval a tile showed it at.
        let mut shown = None;
        for (key, entry) in &track.cache.entries {
            if key.contig != source_contig {
                continue;
            }
            if let Some(record) = entry.records.get(&id) {
                return TrackRecordLookup::Ready(Arc::clone(record));
            }
            if shown.is_none()
                && let Some(record) = entry
                    .tile
                    .scene
                    .records
                    .iter()
                    .find(|record| record.id == id)
            {
                shown = Some((record.start, record.end, key.digest));
            }
        }
        let key: LookupKey = (source_contig, id);
        // 2. An earlier lookup.
        if let Some(index) = track
            .resolved
            .iter()
            .position(|(resolved, _)| *resolved == key)
        {
            let outcome = track.resolved[index].1.clone();
            if !matches!(outcome, TrackRecordLookup::Ready(_)) {
                track.resolved.remove(index);
            }
            return outcome;
        }
        // The same record already asked for, unless that read was cancelled: a cancelled
        // read answers nothing, so asking again schedules a fresh one behind it.
        if track
            .lookup
            .as_ref()
            .is_some_and(|lookup| lookup.key == key && !lookup.cancel.load(Ordering::Acquire))
        {
            return TrackRecordLookup::Pending;
        }
        // 3. A bounded re-read, which needs the record's interval.
        let Some((start, end, digest)) = shown else {
            return TrackRecordLookup::NotFound;
        };
        if matches!(track.source, SourceSlot::Failed) {
            return TrackRecordLookup::Failed(format!("{}: the track is not open", track.name));
        }
        let settings = track
            .views
            .values()
            .find(|view| view.digest == digest)
            .map_or_else(TrackTileSettings::default, |view| view.settings.clone());
        if let Some(previous) = track.lookup.replace(PendingLookup {
            query: TrackRecordQuery {
                contig: key.0.to_string(),
                id,
                start,
                end,
                settings,
            },
            key,
            cancel: Arc::new(AtomicBool::new(false)),
            started: false,
        }) {
            previous.cancel.store(true, Ordering::Release);
        }
        // An inspection is the user waiting: a prefetch of this track gives way to it.
        if let Some(in_flight) = track.in_flight_margin() {
            in_flight.cancel.store(true, Ordering::Release);
        }
        drop(state);
        self.shared.work.notify_all();
        TrackRecordLookup::Pending
    }

    /// [`Self::lookup_record`], waiting up to `timeout` for a pending re-read: for callers
    /// that are not a UI thread (automation, export, tests). Returns
    /// [`TrackRecordLookup::Pending`] when the time runs out; the read keeps going and a
    /// later call collects it.
    pub fn lookup_record_blocking(
        &self,
        track: TrackId,
        contig: &str,
        id: TrackRecordId,
        timeout: std::time::Duration,
    ) -> TrackRecordLookup {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let outcome = self.lookup_record(track, contig, id);
            if !outcome.is_pending() {
                return outcome;
            }
            let state = self.shared.lock();
            let still_pending = state.tracks.get(&track).is_some_and(|track| {
                track.lookup.as_ref().is_some_and(|lookup| {
                    lookup.key.1 == id
                        && *lookup.key.0
                            == *track.aliases.get(contig).map_or(contig, String::as_str)
                })
            });
            if !still_pending {
                continue;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return TrackRecordLookup::Pending;
            }
            drop(
                self.shared
                    .lookups
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }

    /// Abandons the pending record lookup of `track`, cancelling its read if one is running.
    pub fn cancel_record_lookup(&self, track: TrackId) {
        let mut state = self.shared.lock();
        if let Some(track) = state.tracks.get_mut(&track)
            && let Some(lookup) = &track.lookup
        {
            lookup.cancel.store(true, Ordering::Release);
            if !lookup.started {
                track.lookup = None;
            }
        }
        drop(state);
        self.shared.lookups.notify_all();
    }

    #[must_use]
    pub fn cache_stats(&self, track: TrackId) -> Option<TrackTileCacheStats> {
        let state = self.shared.lock();
        let track = state.tracks.get(&track)?;
        Some(TrackTileCacheStats {
            tiles: track.cache.entries.len(),
            bytes: track.cache.bytes,
            budget_bytes: self.shared.cache_bytes_per_track,
            ..track.stats
        })
    }

    /// Open tracks that have skipped unparseable input, ordered by track.
    #[must_use]
    pub fn source_diagnostics(&self) -> Vec<(TrackId, TrackSourceDiagnostics)> {
        self.shared
            .lock()
            .tracks
            .iter()
            .filter_map(|(id, track)| {
                let report = track.diagnostics.clone()?;
                (report.malformed_lines > 0).then_some((*id, report))
            })
            .collect()
    }

    /// Searches every track by name. A newer search cancels this one, including its remote
    /// transfers; only the newest search is ever answered.
    pub fn search(&self, generation: u64, query: String, limit: usize, include_variant_ids: bool) {
        self.start_fan_out(
            generation,
            FanOutKind::Search(TrackTileSearch {
                query,
                limit: limit.min(MAX_FEATURE_SEARCH_RESULTS),
                include_variant_ids,
            }),
        );
    }

    /// Finds the nearest feature after (or before) `position` over every track. A newer step
    /// cancels this one.
    pub fn adjacent_feature(&self, generation: u64, contig: String, position: u64, forward: bool) {
        self.start_fan_out(
            generation,
            FanOutKind::Adjacent {
                contig,
                position,
                forward,
            },
        );
    }

    fn start_fan_out(&self, generation: u64, kind: FanOutKind) {
        let mut events = Vec::new();
        let mut state = self.shared.lock();
        let is_search = matches!(kind, FanOutKind::Search(_));
        let fan_out = FanOut {
            generation,
            kind: Arc::new(kind),
            remaining: state
                .tracks
                .iter()
                .filter(|(_, track)| !matches!(track.source, SourceSlot::Failed))
                .map(|(id, _)| *id)
                .collect(),
            running: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            hits: Vec::new(),
            index_truncated: false,
            scan_truncated: false,
        };
        let slot = if is_search {
            &mut state.search
        } else {
            &mut state.adjacent
        };
        if let Some(previous) = slot.replace(fan_out) {
            previous.cancel.store(true, Ordering::Release);
        }
        finish_fan_out(slot, &mut events);
        drop(state);
        self.shared.work.notify_all();
        self.shared.publish(events);
    }

    pub fn try_receive(&self) -> Result<Option<TrackTileEngineEvent>, EngineQueueError> {
        self.output
            .try_receive()
            .map_err(|_| EngineQueueError::Disconnected)
    }

    #[cfg(test)]
    fn receive_timeout(&self, timeout: std::time::Duration) -> Option<TrackTileEngineEvent> {
        self.output.receive_timeout(timeout).ok().flatten()
    }
}

impl Drop for TrackTileEngine {
    /// Raises every cancellation, wakes the helpers and returns without joining them: a helper
    /// may be inside a transfer that takes a moment to notice. Reaper threads wait instead.
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.shutdown = true;
        for track in state.tracks.values() {
            if let Some(in_flight) = &track.in_flight {
                in_flight.cancel.store(true, Ordering::Release);
            }
            if let Some(lookup) = &track.lookup {
                lookup.cancel.store(true, Ordering::Release);
            }
        }
        for fan_out in [&state.search, &state.adjacent].into_iter().flatten() {
            fan_out.cancel.store(true, Ordering::Release);
        }
        drop(state);
        self.output.close();
        self.shared.work.notify_all();
        self.shared.lookups.notify_all();
        for helper in self.helpers.drain(..) {
            drop(reap_worker(
                helper,
                "rgv-track-tile-reaper",
                "track tile helper",
            ));
        }
    }
}

/// Registers `id`, replacing (and cancelling) an earlier registration under the same id.
fn register_track(
    state: &mut State,
    id: TrackId,
    name: String,
    aliases: BTreeMap<String, String>,
    source: SourceSlot,
) {
    state.next_epoch += 1;
    let epoch = state.next_epoch;
    if let Some(previous) = state
        .tracks
        .insert(id, TrackState::new(epoch, name, aliases, source))
        && let Some(in_flight) = &previous.in_flight
    {
        in_flight.cancel.store(true, Ordering::Release);
    }
}

/// Registers tracks whose source the legacy [`crate::TrackEngine`] owns and shares. Holds the
/// tile engine weakly: sharing with an engine that has been dropped does nothing.
#[derive(Clone)]
pub(crate) struct TrackTileSourceRegistrar {
    shared: std::sync::Weak<Shared>,
}

impl TrackTileSourceRegistrar {
    /// Registers `ids` as awaited, so views may be requested before the legacy engine has
    /// answered. Each must be followed by [`Self::supply`], [`Self::fail`] or
    /// [`Self::withdraw`].
    pub(crate) fn expect(&self, ids: &[TrackId]) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared.lock();
        for id in ids {
            register_track(
                &mut state,
                *id,
                String::new(),
                BTreeMap::new(),
                SourceSlot::Busy,
            );
        }
    }

    /// Forgets awaited tracks whose request never reached the legacy engine.
    pub(crate) fn withdraw(&self, ids: &[TrackId]) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared.lock();
        for id in ids {
            if state
                .tracks
                .get(id)
                .is_some_and(|track| matches!(track.source, SourceSlot::Busy))
            {
                state.tracks.remove(id);
            }
        }
    }

    /// Hands the shared source of `id` to the tile engine.
    pub(crate) fn supply(
        &self,
        id: TrackId,
        name: String,
        aliases: BTreeMap<String, String>,
        source: Box<dyn TrackTileSource>,
    ) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared.lock();
        // Awaited: fill it in, keeping the views requested meanwhile. Otherwise (removed, or
        // re-registered with a source of its own) the latest registration wins.
        let awaited = state
            .tracks
            .get_mut(&id)
            .filter(|track| track.in_flight.is_none() && matches!(track.source, SourceSlot::Busy));
        let Some(track) = awaited else {
            return;
        };
        track.name = name;
        if track.aliases.is_empty() {
            track.aliases = aliases;
        }
        // Views requested while the source was awaited were resolved without its aliases.
        for view in track.views.values_mut() {
            if let Some(source_contig) = track.aliases.get(&*view.contig) {
                view.source_contig = Arc::from(source_contig.as_str());
            }
        }
        track.diagnostics = source.diagnostics();
        track.source = SourceSlot::Idle(source);
        preempt_margin_reads(&mut state);
        drop(state);
        shared.work.notify_all();
        shared.publish(vec![TrackTileEngineEvent::TrackOpened { track: id }]);
    }

    /// Reports that the legacy engine has no open source for `id`.
    pub(crate) fn fail(&self, id: TrackId, message: &str) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut events = Vec::new();
        let mut state = shared.lock();
        let awaited = state
            .tracks
            .get_mut(&id)
            .filter(|track| track.in_flight.is_none() && matches!(track.source, SourceSlot::Busy));
        let Some(track) = awaited else {
            return;
        };
        fail_track_open(id, track, message, &mut events);
        drop(state);
        shared.publish(events);
    }
}

/// Marks `track` as failed to open: every visible tile of every view fails with the message.
fn fail_track_open(
    id: TrackId,
    track: &mut TrackState,
    message: &str,
    events: &mut Vec<TrackTileEngineEvent>,
) {
    track.source = SourceSlot::Failed;
    let message = if track.name.is_empty() {
        message.to_owned()
    } else {
        format!("{}: {message}", track.name)
    };
    track.failure = Some(message.clone());
    for (panel, view) in &mut track.views {
        for tile in view.visible.clone() {
            view.failed.insert(tile, message.clone());
            events.push(TrackTileEngineEvent::TileFailed {
                panel: *panel,
                track: id,
                generation: view.generation,
                key: tile,
                message: message.clone(),
            });
        }
    }
    settle_views(id, track, events);
    events.push(TrackTileEngineEvent::TrackOpenFailed { track: id, message });
}

/// Makes room for visible tiles that have no free helper.
///
/// * A track whose own in-flight read is a tile no view shows, while one of its views waits
///   for a visible tile, gives that read up: the source can do only one thing at a time.
/// * When more tracks have a visible tile ready to read than helpers are waiting, that many
///   in-flight reads of tiles no view shows (margin tiles) are cancelled on other tracks.
///
/// A cancelled margin tile stays wanted and is read again once the visible tiles are served.
/// Cancellation takes effect when the source next polls it; sources that never poll are
/// bounded by [`margin_read_limit`] instead.
fn preempt_margin_reads(state: &mut State) {
    let mut runnable_visible = 0_usize;
    for track in state.tracks.values() {
        let visible_pending = next_tile(track, true).is_some();
        if !visible_pending {
            continue;
        }
        if track.in_flight.is_none() && matches!(track.source, SourceSlot::Idle(_)) {
            runnable_visible += 1;
        } else if let Some(in_flight) = track.in_flight_margin() {
            in_flight.cancel.store(true, Ordering::Release);
        }
    }
    let mut short = runnable_visible.saturating_sub(state.waiting);
    for track in state.tracks.values() {
        if short == 0 {
            break;
        }
        if let Some(in_flight) = track.in_flight_margin()
            && !in_flight.cancel.swap(true, Ordering::AcqRel)
        {
            short -= 1;
        }
    }
}

/// Margin reads in flight that have not been cancelled.
fn margin_reads_in_flight(state: &State) -> usize {
    state
        .tracks
        .values()
        .filter(|track| {
            track
                .in_flight_margin()
                .is_some_and(|in_flight| !in_flight.cancel.load(Ordering::Acquire))
        })
        .count()
}

/// Emits `ViewSettled` for every view of `track` whose visible tiles are all ready or failed.
fn settle_views(id: TrackId, track: &mut TrackState, events: &mut Vec<TrackTileEngineEvent>) {
    let TrackState { views, cache, .. } = track;
    for (panel, view) in views.iter_mut() {
        if view.settled {
            continue;
        }
        let settled = view.visible.iter().all(|tile| {
            view.failed.contains_key(tile) || cache.entries.contains_key(&view.cache_key(*tile))
        });
        if settled {
            view.settled = true;
            events.push(TrackTileEngineEvent::ViewSettled {
                panel: *panel,
                track: id,
                generation: view.generation,
            });
        }
    }
}

/// Publishes and clears a fan-out whose every part has finished.
fn finish_fan_out(slot: &mut Option<FanOut>, events: &mut Vec<TrackTileEngineEvent>) {
    if !slot
        .as_ref()
        .is_some_and(|fan_out| fan_out.remaining.is_empty() && fan_out.running == 0)
    {
        return;
    }
    let Some(mut fan_out) = slot.take() else {
        return;
    };
    match &*fan_out.kind {
        FanOutKind::Search(search) => {
            let normalized = search.query.trim().to_lowercase();
            fan_out.hits.sort_by(|left, right| {
                let left_exact = left.matched_term == normalized;
                let right_exact = right.matched_term == normalized;
                right_exact
                    .cmp(&left_exact)
                    .then_with(|| left.matched_term.cmp(&right.matched_term))
                    .then_with(|| left.record.contig.cmp(&right.record.contig))
                    .then_with(|| left.record.start.cmp(&right.record.start))
                    .then_with(|| left.record.end.cmp(&right.record.end))
                    .then_with(|| left.track_id.cmp(&right.track_id))
            });
            fan_out.hits.dedup_by(|left, right| {
                left.track_id == right.track_id
                    && left.record.contig == right.record.contig
                    && left.record.start == right.record.start
                    && left.record.end == right.record.end
                    && left.record.name == right.record.name
            });
            fan_out.hits.truncate(search.limit);
            events.push(TrackTileEngineEvent::SearchCompleted {
                generation: fan_out.generation,
                results: TrackSearchResults {
                    query: search.query.clone(),
                    hits: fan_out.hits,
                    index_truncated: fan_out.index_truncated,
                    scan_truncated: fan_out.scan_truncated,
                    variant_ids_included: search.include_variant_ids,
                },
            });
        }
        FanOutKind::Adjacent { forward, .. } => {
            fan_out.hits.sort_by(|left, right| {
                let ordering = left
                    .record
                    .start
                    .cmp(&right.record.start)
                    .then_with(|| left.record.end.cmp(&right.record.end))
                    .then_with(|| left.track_id.cmp(&right.track_id));
                if *forward {
                    ordering
                } else {
                    ordering.reverse()
                }
            });
            events.push(TrackTileEngineEvent::AdjacentFeatureCompleted {
                generation: fan_out.generation,
                hit: fan_out.hits.into_iter().next().map(Box::new),
            });
        }
    }
}

enum JobWork {
    Open(TrackSpec),
    Tile {
        source: Box<dyn TrackTileSource>,
        request: TrackTileRequest,
        key: CacheKey,
    },
    FanOut {
        source: Box<dyn TrackTileSource>,
        kind: Arc<FanOutKind>,
        generation: u64,
        is_search: bool,
        /// Contig of an adjacent-feature step as this track's source spells it.
        source_contig: Option<String>,
    },
    Lookup {
        source: Box<dyn TrackTileSource>,
        key: LookupKey,
        query: TrackRecordQuery,
    },
}

struct Job {
    track: TrackId,
    /// Registration the operation was started for.
    epoch: u64,
    cancel: Arc<AtomicBool>,
    work: JobWork,
}

enum JobOutcome {
    Opened(Result<Box<dyn TrackTileSource>, String>),
    Tile {
        source: Option<Box<dyn TrackTileSource>>,
        key: CacheKey,
        result: Result<(Arc<LaidOutTrackTile>, RetainedRecords), TrackTileError>,
    },
    FanOut {
        source: Option<Box<dyn TrackTileSource>>,
        generation: u64,
        is_search: bool,
        result: Result<FanOutPart, TrackTileError>,
    },
    Lookup {
        source: Option<Box<dyn TrackTileSource>>,
        key: LookupKey,
        result: Result<Option<TrackRecord>, TrackTileError>,
    },
}

enum FanOutPart {
    Search(Option<TrackTileSearchHits>),
    Adjacent(Option<FeatureRecord>),
}

/// Tracks in round-robin order, starting after the one served last.
fn round_robin(state: &State) -> Vec<TrackId> {
    let Some(cursor) = state.cursor else {
        return state.tracks.keys().copied().collect();
    };
    state
        .tracks
        .range((
            std::ops::Bound::Excluded(cursor),
            std::ops::Bound::Unbounded,
        ))
        .chain(state.tracks.range(..=cursor))
        .map(|(id, _)| *id)
        .collect()
}

/// The next tile `track` should read for its views: visible ones when `visible`, else margin.
fn next_tile(track: &TrackState, visible: bool) -> Option<(CacheKey, TrackTileRequest)> {
    track.views.values().find_map(|view| {
        let tiles = if visible { &view.visible } else { &view.margin };
        tiles
            .iter()
            .filter(|tile| !view.failed.contains_key(tile))
            .map(|tile| view.cache_key(*tile))
            .find(|key| !track.cache.entries.contains_key(key))
            .map(|key| {
                let request = TrackTileRequest::new(&*key.contig, key.tile, view.settings.clone());
                (key, request)
            })
    })
}

/// Picks the most urgent runnable operation and marks its track busy.
#[allow(clippy::too_many_lines)]
fn next_job(state: &mut State) -> Option<Job> {
    let order = round_robin(state);
    let idle = |track: &TrackState| {
        track.in_flight.is_none() && matches!(track.source, SourceSlot::Idle(_))
    };
    let take_source =
        |track: &mut TrackState| match std::mem::replace(&mut track.source, SourceSlot::Busy) {
            SourceSlot::Idle(source) => source,
            _ => unreachable!("only idle sources are lent"),
        };
    // 1. Sources to open.
    for id in &order {
        let track = state.tracks.get_mut(id)?;
        if track.in_flight.is_none() && matches!(track.source, SourceSlot::Unopened(_)) {
            let SourceSlot::Unopened(specification) =
                std::mem::replace(&mut track.source, SourceSlot::Busy)
            else {
                unreachable!();
            };
            let cancel = Arc::new(AtomicBool::new(false));
            track.in_flight = Some(InFlight {
                cancel: Arc::clone(&cancel),
                tile: None,
            });
            let epoch = track.epoch;
            state.cursor = Some(*id);
            return Some(Job {
                track: *id,
                epoch,
                cancel,
                work: JobWork::Open(specification),
            });
        }
    }
    // 2. Search and feature-step parts.
    for is_search in [true, false] {
        let State {
            tracks,
            search,
            adjacent,
            cursor,
            ..
        } = state;
        let Some(fan_out) = (if is_search { search } else { adjacent }).as_mut() else {
            continue;
        };
        // A track that failed to open or was removed has no part to play.
        fan_out.remaining.retain(|id| {
            tracks
                .get(id)
                .is_some_and(|track| !matches!(track.source, SourceSlot::Failed))
        });
        for id in &order {
            if !fan_out.remaining.contains(id) {
                continue;
            }
            let Some(track) = tracks.get_mut(id) else {
                continue;
            };
            if !idle(track) {
                continue;
            }
            fan_out.remaining.remove(id);
            fan_out.running += 1;
            track.in_flight = Some(InFlight {
                cancel: Arc::clone(&fan_out.cancel),
                tile: None,
            });
            let source_contig = match &*fan_out.kind {
                FanOutKind::Adjacent { contig, .. } => {
                    Some(track.aliases.get(contig).unwrap_or(contig).clone())
                }
                FanOutKind::Search(_) => None,
            };
            *cursor = Some(*id);
            return Some(Job {
                track: *id,
                epoch: track.epoch,
                cancel: Arc::clone(&fan_out.cancel),
                work: JobWork::FanOut {
                    source: take_source(track),
                    kind: Arc::clone(&fan_out.kind),
                    generation: fan_out.generation,
                    is_search,
                    source_contig,
                },
            });
        }
    }
    // 2b. Record lookups: one bounded read each, for a user who is waiting.
    for id in &order {
        let Some(track) = state.tracks.get_mut(id) else {
            continue;
        };
        if !idle(track) {
            continue;
        }
        let Some(lookup) = track.lookup.as_mut().filter(|lookup| !lookup.started) else {
            continue;
        };
        lookup.started = true;
        let (key, query, cancel) = (
            lookup.key.clone(),
            lookup.query.clone(),
            Arc::clone(&lookup.cancel),
        );
        track.in_flight = Some(InFlight {
            cancel: Arc::clone(&cancel),
            tile: None,
        });
        let epoch = track.epoch;
        state.cursor = Some(*id);
        let source = take_source(state.tracks.get_mut(id)?);
        return Some(Job {
            track: *id,
            epoch,
            cancel,
            work: JobWork::Lookup { source, key, query },
        });
    }
    // 3. Visible tiles of every track, then 4. margins, which never take the whole pool.
    for visible in [true, false] {
        if !visible && margin_reads_in_flight(state) >= margin_read_limit(state.helpers) {
            break;
        }
        for id in &order {
            let track = state.tracks.get_mut(id)?;
            if !idle(track) {
                continue;
            }
            let Some((key, request)) = next_tile(track, visible) else {
                continue;
            };
            let cancel = Arc::new(AtomicBool::new(false));
            track.in_flight = Some(InFlight {
                cancel: Arc::clone(&cancel),
                tile: Some(key.clone()),
            });
            let epoch = track.epoch;
            state.cursor = Some(*id);
            return Some(Job {
                track: *id,
                epoch,
                cancel,
                work: JobWork::Tile {
                    source: take_source(track),
                    request,
                    key,
                },
            });
        }
    }
    None
}

/// Runs one operation with no lock held. A panic inside a source is contained here: it becomes
/// that operation's error, and the source is dropped rather than reused in an unknown state.
#[allow(clippy::too_many_lines)]
fn run_job(shared: &Shared, job: JobWork, cancel: &TrackCancellation) -> JobOutcome {
    match job {
        JobWork::Open(specification) => {
            let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if matches!(
                    specification.kind,
                    TrackSpecKind::Tdf | TrackSpecKind::TdfChannel { .. }
                ) {
                    let mut tdf_sources = shared
                        .tdf_sources
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    open_tile_source(&specification, &mut tdf_sources, &shared.remote)
                } else {
                    open_tile_source(&specification, &mut HashMap::new(), &shared.remote)
                }
            }));
            JobOutcome::Opened(opened.unwrap_or_else(|payload| {
                Err(format!(
                    "internal error: {}",
                    panic_message(payload.as_ref())
                ))
            }))
        }
        JobWork::Tile {
            mut source,
            request,
            key,
        } => {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let tile = source.query_tile(&request, cancel)?;
                let records = source
                    .take_retained_records()
                    .into_iter()
                    .map(|record| (record.id(), Arc::new(record)))
                    .collect::<RetainedRecords>();
                cancel.check()?;
                let mut scene = shared.layouts.lay_out(&tile)?;
                scene.apply_row_cap(request.settings.maximum_feature_rows);
                Ok((
                    Arc::new(LaidOutTrackTile {
                        tile,
                        scene,
                        settings_digest: key.digest,
                    }),
                    records,
                ))
            }));
            match result {
                Ok(result) => JobOutcome::Tile {
                    source: Some(source),
                    key,
                    result,
                },
                Err(payload) => JobOutcome::Tile {
                    source: None,
                    key,
                    result: Err(TrackTileError::Source(format!(
                        "internal error: {}",
                        panic_message(payload.as_ref())
                    ))),
                },
            }
        }
        JobWork::Lookup {
            mut source,
            key,
            query,
        } => {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                source.lookup_record(&query, cancel)
            }));
            match result {
                Ok(result) => JobOutcome::Lookup {
                    source: Some(source),
                    key,
                    result,
                },
                Err(payload) => JobOutcome::Lookup {
                    source: None,
                    key,
                    result: Err(TrackTileError::Source(format!(
                        "internal error: {}",
                        panic_message(payload.as_ref())
                    ))),
                },
            }
        }
        JobWork::FanOut {
            mut source,
            kind,
            generation,
            is_search,
            source_contig,
        } => {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &*kind {
                FanOutKind::Search(search) => source.search(search, cancel).map(FanOutPart::Search),
                FanOutKind::Adjacent {
                    position, forward, ..
                } => source
                    .adjacent_feature(
                        source_contig.as_deref().unwrap_or_default(),
                        *position,
                        *forward,
                        cancel,
                    )
                    .map(FanOutPart::Adjacent),
            }));
            match result {
                Ok(result) => JobOutcome::FanOut {
                    source: Some(source),
                    generation,
                    is_search,
                    result,
                },
                Err(payload) => JobOutcome::FanOut {
                    source: None,
                    generation,
                    is_search,
                    result: Err(TrackTileError::Source(format!(
                        "internal error: {}",
                        panic_message(payload.as_ref())
                    ))),
                },
            }
        }
    }
}

/// Returns the source to its track and records what the operation produced.
#[allow(clippy::too_many_lines)]
fn complete_job(
    shared: &Shared,
    state: &mut State,
    id: TrackId,
    epoch: u64,
    outcome: JobOutcome,
    events: &mut Vec<TrackTileEngineEvent>,
) {
    // The registration the operation belongs to; `None` once it was removed or replaced.
    let current = |state: &State| {
        state
            .tracks
            .get(&id)
            .is_some_and(|track| track.epoch == epoch)
    };
    // The fan-out bookkeeping happens whether or not the track still exists.
    let (returned, tile_outcome) = match outcome {
        JobOutcome::Opened(result) => {
            if !current(state) {
                return;
            }
            let Some(track) = state.tracks.get_mut(&id) else {
                return;
            };
            track.in_flight = None;
            match result {
                Ok(source) => {
                    track.diagnostics = source.diagnostics();
                    track.source = SourceSlot::Idle(source);
                    events.push(TrackTileEngineEvent::TrackOpened { track: id });
                }
                Err(message) => fail_track_open(id, track, &message, events),
            }
            return;
        }
        JobOutcome::FanOut {
            source,
            generation,
            is_search,
            result,
        } => {
            let name = state
                .tracks
                .get(&id)
                .map(|track| track.name.clone())
                .unwrap_or_default();
            let slot = if is_search {
                &mut state.search
            } else {
                &mut state.adjacent
            };
            // A superseded fan-out was replaced; its late parts are simply dropped.
            if let Some(fan_out) = slot
                .as_mut()
                .filter(|fan_out| fan_out.generation == generation)
            {
                fan_out.running -= 1;
                let mut hit = |matched_term: String, record: FeatureRecord| {
                    fan_out.hits.push(TrackSearchHit {
                        track_id: id,
                        track_name: name.clone(),
                        matched_term,
                        record,
                    });
                };
                match result {
                    Ok(FanOutPart::Search(Some(results))) => {
                        fan_out.index_truncated |= results.index_truncated;
                        fan_out.scan_truncated |= results.scan_truncated;
                        for (matched_term, record) in results.hits {
                            hit(matched_term, record);
                        }
                    }
                    Ok(FanOutPart::Adjacent(Some(record))) => hit(
                        record.name.as_deref().unwrap_or_default().to_lowercase(),
                        record,
                    ),
                    Ok(FanOutPart::Search(None) | FanOutPart::Adjacent(None))
                    | Err(TrackTileError::Cancelled) => {}
                    Err(error) => {
                        tracing::warn!(%error, track = %name, "track search or navigation failed");
                    }
                }
                finish_fan_out(slot, events);
            }
            (source, None)
        }
        JobOutcome::Lookup {
            source,
            key,
            result,
        } => {
            if current(state)
                && let Some(track) = state.tracks.get_mut(&id)
            {
                // Only the lookup still asked for is answered. A superseded or cancelled one
                // keeps nothing but a record it happened to find.
                let asked = track
                    .lookup
                    .as_ref()
                    .is_some_and(|lookup| lookup.started && lookup.key == key);
                if asked {
                    track.lookup = None;
                }
                match result {
                    Ok(Some(record)) => {
                        // A fresh read of the same record queued behind this one is moot.
                        if track
                            .lookup
                            .as_ref()
                            .is_some_and(|lookup| !lookup.started && lookup.key == key)
                        {
                            track.lookup = None;
                        }
                        track.resolve(key, TrackRecordLookup::Ready(Arc::new(record)));
                    }
                    Ok(None) if asked => track.resolve(key, TrackRecordLookup::NotFound),
                    Err(error) if asked && error != TrackTileError::Cancelled => {
                        let message = format!("{}: {error}", track.name);
                        track.resolve(key, TrackRecordLookup::Failed(message));
                    }
                    Ok(None) | Err(_) => {}
                }
            }
            shared.lookups.notify_all();
            crate::mailbox::wake_ui();
            (source, None)
        }
        JobOutcome::Tile {
            source,
            key,
            result,
        } => (source, Some((key, result))),
    };
    if !current(state) {
        // The source belongs to a registration that is gone; it is dropped here.
        return;
    }
    let Some(track) = state.tracks.get_mut(&id) else {
        return;
    };
    track.in_flight = None;
    match returned {
        Some(source) => {
            track.diagnostics = source.diagnostics();
            track.source = SourceSlot::Idle(source);
        }
        // The source panicked; it is not reused.
        None => track.source = SourceSlot::Failed,
    }
    let Some((key, result)) = tile_outcome else {
        return;
    };
    match result {
        Ok((tile, records)) => {
            track.stats.completed += 1;
            let TrackState {
                cache,
                views,
                stats,
                ..
            } = track;
            // A complete tile is cached even if its view has moved on: it is viewport
            // independent and the user may pan back.
            stats.evictions += cache.insert(
                key.clone(),
                tile,
                records,
                shared.cache_bytes_per_track,
                &|key| views.values().any(|view| view.wants(key)),
            );
            for (panel, view) in views.iter() {
                if view.wants(&key) {
                    events.push(TrackTileEngineEvent::TileReady {
                        panel: *panel,
                        track: id,
                        generation: view.generation,
                        key: key.tile,
                    });
                }
            }
        }
        Err(TrackTileError::Cancelled) => track.stats.cancelled += 1,
        Err(error) => {
            track.stats.failed += 1;
            let message = format!("{}: {error}", track.name);
            for (panel, view) in &mut track.views {
                if view.wants(&key) {
                    view.failed.insert(key.tile, message.clone());
                    events.push(TrackTileEngineEvent::TileFailed {
                        panel: *panel,
                        track: id,
                        generation: view.generation,
                        key: key.tile,
                        message: message.clone(),
                    });
                }
            }
        }
    }
    settle_views(id, track, events);
}

fn helper_loop(shared: &Shared) {
    loop {
        let job = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(job) = next_job(&mut state) {
                    break job;
                }
                state.waiting += 1;
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.waiting -= 1;
            }
        };
        let cancel = TrackCancellation::from_flag(Arc::clone(&job.cancel));
        let outcome = run_job(shared, job.work, &cancel);
        let mut events = Vec::new();
        {
            let mut state = shared.lock();
            if state.shutdown {
                return;
            }
            complete_job(
                shared,
                &mut state,
                job.track,
                job.epoch,
                outcome,
                &mut events,
            );
            // An opened source or a finished read may have made a visible tile runnable.
            preempt_margin_reads(&mut state);
        }
        // The track is free again and a fan-out may have become runnable.
        shared.work.notify_all();
        shared.publish(events);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp, clippy::too_many_lines, clippy::format_push_string)]

    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    use rgv_model::{
        BinSemantics, BinTile, FeatureRowAxis, FeatureTile, Strand, TileBin, TileFeature,
        TrackTileLod, TrackTilePayload, TrackTileProvenance,
    };

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// A source whose every tile holds one feature spanning the tile, with optional gating so
    /// a test can hold a read open and observe its cancellation.
    #[derive(Clone, Default)]
    struct Probe {
        queries: Arc<Mutex<Vec<TrackTileKey>>>,
        cancelled: Arc<AtomicUsize>,
        /// When set, a read blocks until released or cancelled.
        gate: Option<Arc<AtomicBool>>,
        started: Option<mpsc::SyncSender<TrackTileKey>>,
        /// A gated read does not poll its cancellation, like a source blocked in a system
        /// call.
        deaf: bool,
        fail: bool,
        panic: bool,
        payload_bytes: usize,
    }

    impl TrackTileSource for Probe {
        fn query_tile(
            &mut self,
            request: &TrackTileRequest,
            cancel: &TrackCancellation,
        ) -> Result<TrackTile, TrackTileError> {
            self.queries.lock().unwrap().push(request.tile);
            if let Some(started) = &self.started {
                let _ = started.try_send(request.tile);
            }
            if let Some(gate) = &self.gate {
                let deadline = Instant::now() + WAIT;
                while !gate.load(Ordering::Acquire) {
                    if !self.deaf && cancel.is_cancelled() {
                        self.cancelled.fetch_add(1, Ordering::AcqRel);
                        return Err(TrackTileError::Cancelled);
                    }
                    assert!(Instant::now() < deadline, "gated read was never released");
                    thread::sleep(Duration::from_millis(1));
                }
            }
            assert!(!self.panic, "probe source panicked");
            if self.fail {
                return Err(TrackTileError::Source("unreadable".to_owned()));
            }
            let payload = if self.payload_bytes > 0 {
                TrackTilePayload::Bins(BinTile {
                    bin_width: request.tile.nominal_bases_per_point(),
                    bins: vec![
                        TileBin {
                            minimum: 1.0,
                            mean: 1.0,
                            maximum: 1.0
                        };
                        self.payload_bytes / size_of::<TileBin>()
                    ],
                    semantics: BinSemantics::Signal,
                })
            } else {
                TrackTilePayload::Features(FeatureTile {
                    features: vec![TileFeature {
                        id: TrackRecordId(request.tile.index),
                        start: request.tile.start(),
                        end: request.tile.end(),
                        strand: Strand::Forward,
                        blocks: Vec::new(),
                        name: None,
                        score: None,
                        color: None,
                        row_hint: Some(0),
                        labelable: false,
                    }],
                    rows: FeatureRowAxis::Packed,
                })
            };
            Ok(TrackTile {
                contig: request.contig.clone(),
                key: request.tile,
                lod: TrackTileLod::Detail,
                payload,
                provenance: TrackTileProvenance::default(),
            })
        }

        fn search(
            &mut self,
            search: &TrackTileSearch,
            cancel: &TrackCancellation,
        ) -> Result<Option<TrackTileSearchHits>, TrackTileError> {
            if let Some(gate) = &self.gate {
                while !gate.load(Ordering::Acquire) {
                    if cancel.is_cancelled() {
                        self.cancelled.fetch_add(1, Ordering::AcqRel);
                        return Err(TrackTileError::Cancelled);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(Some(TrackTileSearchHits {
                hits: vec![(
                    search.query.to_lowercase(),
                    FeatureRecord {
                        contig: "chr1".to_owned(),
                        start: 10,
                        end: 20,
                        name: Some(search.query.clone()),
                        feature_type: None,
                        score: None,
                        strand: Strand::Unstranded,
                        blocks: Vec::new(),
                        coding_interval: None,
                        peak: None,
                        attributes: Vec::new(),
                    },
                )],
                index_truncated: false,
                scan_truncated: false,
            }))
        }
    }

    fn engine(helpers: usize, margin_tiles: u64, cache_bytes_per_track: usize) -> TrackTileEngine {
        TrackTileEngine::spawn(TrackTileEngineConfig {
            helpers,
            cache_bytes_per_track,
            margin_tiles,
            ..TrackTileEngineConfig::default()
        })
    }

    fn view(track: u64, start: f64, end: f64, bases_per_point: f64) -> TrackTileView {
        TrackTileView {
            panel: 1,
            track: TrackId::new(track),
            contig: "chr1".to_owned(),
            start,
            end,
            bases_per_point,
            contig_length: None,
            settings: TrackTileSettings::default(),
        }
    }

    /// Receives events until `done` accepts one; panics after [`WAIT`].
    fn wait_for(
        engine: &TrackTileEngine,
        mut done: impl FnMut(&TrackTileEngineEvent) -> bool,
    ) -> Vec<TrackTileEngineEvent> {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "timed out; saw {seen:?}");
            if let Some(event) = engine.receive_timeout(Duration::from_millis(20)) {
                let finished = done(&event);
                seen.push(event);
                if finished {
                    return seen;
                }
            }
        }
    }

    fn settled(track: u64, generation: u64) -> impl FnMut(&TrackTileEngineEvent) -> bool {
        move |event| {
            *event
                == TrackTileEngineEvent::ViewSettled {
                    panel: 1,
                    track: TrackId::new(track),
                    generation,
                }
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !condition() {
            assert!(Instant::now() < deadline, "condition never held");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn visible_tiles_load_before_the_margin_and_panning_inside_them_is_free() {
        let probe = Probe::default();
        let engine = engine(1, 1, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        // 1 base per point: level 0; [1500, 3000) covers tiles 1 and 2.
        let generation = engine.request_view(view(1, 1_500.0, 3_000.0, 1.0)).unwrap();
        let events = wait_for(&engine, settled(1, generation));
        let ready = events
            .iter()
            .filter(|event| matches!(event, TrackTileEngineEvent::TileReady { .. }))
            .count();
        assert_eq!(ready, 2);
        let snapshot = engine.view(1, TrackId::new(1)).unwrap();
        assert!(snapshot.is_settled());
        assert_eq!(
            snapshot
                .tiles
                .iter()
                .map(|tile| tile.tile.key.index)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        // Laid out off the caller's thread, in world space, once.
        assert_eq!(snapshot.tiles[0].scene.instances.len(), 1);
        assert_eq!(snapshot.tiles[0].scene.instances[0].start, 0);
        // The margin (tiles 3 and 0) follows the visible tiles, nearest first.
        wait_until(|| probe.queries.lock().unwrap().len() == 4);
        let order = probe
            .queries
            .lock()
            .unwrap()
            .iter()
            .map(|key| key.index)
            .collect::<Vec<_>>();
        assert_eq!(order, [1, 2, 3, 0]);

        // Panning within the same tiles: no read, settled at once from the cache.
        let panned = engine.request_view(view(1, 1_700.0, 3_050.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, panned));
        // Panning onto the prefetched margin: still no visible read.
        let onto_margin = engine.request_view(view(1, 2_100.0, 3_600.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, onto_margin));
        let stats = engine.cache_stats(TrackId::new(1)).unwrap();
        assert_eq!((stats.hits, stats.misses), (4, 2));
        // Only the new margin tile 4 is read.
        wait_until(|| probe.queries.lock().unwrap().len() == 5);
        assert_eq!(probe.queries.lock().unwrap()[4].index, 4);
    }

    #[test]
    fn zooming_selects_the_level_from_bases_per_point() {
        let probe = Probe::default();
        let engine = engine(1, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        let generation = engine.request_view(view(1, 0.0, 100_000.0, 100.0)).unwrap();
        wait_for(&engine, settled(1, generation));
        let snapshot = engine.view(1, TrackId::new(1)).unwrap();
        // 100 bases per point: level 7 (128 per point), tiles of 131,072 bases.
        assert_eq!(snapshot.level, 7);
        assert_eq!(snapshot.tiles.len(), 1);
        assert_eq!(
            snapshot.tiles[0].tile.key,
            TrackTileKey { level: 7, index: 0 }
        );
        // Zooming in keeps the coarse tile as a fallback while the fine level loads.
        let gate = Arc::new(AtomicBool::new(false));
        let gated = Probe {
            gate: Some(Arc::clone(&gate)),
            ..Probe::default()
        };
        engine.add_source(TrackId::new(2), "gated".to_owned(), Box::new(gated));
        gate.store(true, Ordering::Release);
        let coarse = engine.request_view(view(2, 0.0, 100_000.0, 100.0)).unwrap();
        wait_for(&engine, settled(2, coarse));
        gate.store(false, Ordering::Release);
        engine.request_view(view(2, 0.0, 1_000.0, 1.0)).unwrap();
        let loading = engine.view(1, TrackId::new(2)).unwrap();
        assert_eq!(loading.missing, [TrackTileKey { level: 0, index: 0 }]);
        assert_eq!(loading.fallback.len(), 1);
        assert_eq!(loading.fallback[0].tile.key.level, 7);
        gate.store(true, Ordering::Release);
    }

    #[test]
    fn a_slow_track_does_not_delay_another_track_and_is_cancelled_by_the_next_view() {
        let gate = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(8);
        let slow = Probe {
            gate: Some(Arc::clone(&gate)),
            started: Some(started_tx),
            ..Probe::default()
        };
        let fast = Probe::default();
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "slow".to_owned(), Box::new(slow.clone()));
        engine.add_source(TrackId::new(2), "fast".to_owned(), Box::new(fast));
        engine.request_view(view(1, 0.0, 1_000.0, 1.0)).unwrap();
        assert_eq!(started_rx.recv_timeout(WAIT).unwrap().index, 0);
        // The slow read is in flight and stays there; the fast track settles regardless.
        let fast_generation = engine.request_view(view(2, 0.0, 1_000.0, 1.0)).unwrap();
        wait_for(&engine, settled(2, fast_generation));
        assert!(!engine.view(1, TrackId::new(1)).unwrap().is_settled());

        // The slow track's view moves to tile 50: the read of tile 0 is cancelled at once
        // (not when it finishes) and the helper goes straight to the new tile.
        let moved = engine
            .request_view(view(1, 51_200.0, 52_000.0, 1.0))
            .unwrap();
        assert_eq!(started_rx.recv_timeout(WAIT).unwrap().index, 50);
        assert_eq!(slow.cancelled.load(Ordering::Acquire), 1);
        gate.store(true, Ordering::Release);
        let events = wait_for(&engine, settled(1, moved));
        // Nothing was published for the abandoned tile.
        assert!(!events.iter().any(|event| matches!(
            event,
            TrackTileEngineEvent::TileReady { key, track, .. }
                if key.index == 0 && *track == TrackId::new(1)
        )));
        let stats = engine.cache_stats(TrackId::new(1)).unwrap();
        assert_eq!((stats.cancelled, stats.completed, stats.tiles), (1, 1, 1));
    }

    #[test]
    fn a_read_the_new_view_still_wants_survives_and_is_delivered_under_the_new_generation() {
        let gate = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(8);
        let probe = Probe {
            gate: Some(Arc::clone(&gate)),
            started: Some(started_tx),
            ..Probe::default()
        };
        let engine = engine(1, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        engine.request_view(view(1, 0.0, 500.0, 1.0)).unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        // A small pan: the same tile is still wanted.
        let latest = engine.request_view(view(1, 100.0, 600.0, 1.0)).unwrap();
        gate.store(true, Ordering::Release);
        let events = wait_for(&engine, settled(1, latest));
        assert!(events.contains(&TrackTileEngineEvent::TileReady {
            panel: 1,
            track: TrackId::new(1),
            generation: latest,
            key: TrackTileKey { level: 0, index: 0 },
        }));
        assert_eq!(probe.cancelled.load(Ordering::Acquire), 0);
        assert_eq!(probe.queries.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_failing_or_panicking_track_reports_its_own_tiles_and_spares_the_others() {
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let failing = Probe {
            fail: true,
            ..Probe::default()
        };
        let panicking = Probe {
            panic: true,
            ..Probe::default()
        };
        engine.add_source(TrackId::new(1), "failing".to_owned(), Box::new(failing));
        engine.add_source(TrackId::new(2), "panicking".to_owned(), Box::new(panicking));
        engine.add_source(
            TrackId::new(3),
            "healthy".to_owned(),
            Box::new(Probe::default()),
        );
        let generations =
            [1, 2, 3].map(|track| engine.request_view(view(track, 0.0, 1_000.0, 1.0)).unwrap());
        let mut pending = BTreeSet::from([1_u64, 2, 3]);
        let events = wait_for(&engine, |event| {
            if let TrackTileEngineEvent::ViewSettled { track, .. } = event {
                pending.remove(&track.get());
            }
            pending.is_empty()
        });
        let failures = events
            .iter()
            .filter_map(|event| match event {
                TrackTileEngineEvent::TileFailed { track, message, .. } => {
                    Some((track.get(), message.clone()))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(failures[&1], "failing: unreadable");
        assert!(failures[&2].contains("internal error: probe source panicked"));
        assert!(!failures.contains_key(&3));
        let healthy = engine.view(1, TrackId::new(3)).unwrap();
        assert_eq!(
            (healthy.generation, healthy.tiles.len()),
            (generations[2], 1)
        );
        let failed = engine.view(1, TrackId::new(1)).unwrap();
        assert!(failed.is_settled());
        assert_eq!(failed.failed.len(), 1);
    }

    #[test]
    fn the_cache_evicts_least_recently_used_tiles_over_its_byte_budget_but_never_wanted_ones() {
        let probe = Probe {
            payload_bytes: 12_000,
            ..Probe::default()
        };
        // Each tile is a little over 12 kB; the budget holds two.
        let engine = engine(1, 0, 30_000);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        for index in 0..4_u32 {
            let start = f64::from(index) * 1_024.0;
            let generation = engine
                .request_view(view(1, start, start + 500.0, 1.0))
                .unwrap();
            wait_for(&engine, settled(1, generation));
        }
        let stats = engine.cache_stats(TrackId::new(1)).unwrap();
        assert_eq!((stats.tiles, stats.evictions), (2, 2));
        assert!(stats.bytes <= stats.budget_bytes, "{stats:?}");
        // The two newest tiles survived; going back to tile 0 reads it again.
        let back = engine.request_view(view(1, 0.0, 500.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, back));
        assert_eq!(probe.queries.lock().unwrap().len(), 5);
        // A view wider than the budget keeps all its tiles: wanted tiles are pinned.
        let wide = engine.request_view(view(1, 0.0, 4_096.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, wide));
        let snapshot = engine.view(1, TrackId::new(1)).unwrap();
        assert_eq!(snapshot.tiles.len(), 4);
        assert!(engine.cache_stats(TrackId::new(1)).unwrap().bytes > 30_000);
    }

    #[test]
    fn a_settings_change_is_a_different_tile() {
        let probe = Probe::default();
        let engine = engine(1, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        let first = engine.request_view(view(1, 0.0, 500.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, first));
        let mut changed = view(1, 0.0, 500.0, 1.0);
        changed.settings.feature_summary_features_per_point = 3.0;
        let second = engine.request_view(changed).unwrap();
        wait_for(&engine, settled(1, second));
        assert_eq!(probe.queries.lock().unwrap().len(), 2);
        let snapshot = engine.view(1, TrackId::new(1)).unwrap();
        assert_ne!(
            snapshot.tiles[0].settings_digest,
            TrackTileSettings::default().digest()
        );
    }

    #[test]
    fn a_newer_search_cancels_the_running_one_and_only_the_newest_is_answered() {
        let gate = Arc::new(AtomicBool::new(false));
        let slow = Probe {
            gate: Some(Arc::clone(&gate)),
            ..Probe::default()
        };
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "slow".to_owned(), Box::new(slow.clone()));
        engine.add_source(
            TrackId::new(2),
            "fast".to_owned(),
            Box::new(Probe::default()),
        );
        engine.search(1, "BRCA".to_owned(), 10, false);
        // The first search is inside the slow source; the second cancels it there.
        wait_until(|| {
            engine
                .shared
                .lock()
                .search
                .as_ref()
                .is_some_and(|search| search.running > 0)
        });
        engine.search(2, "TP53".to_owned(), 10, false);
        wait_until(|| slow.cancelled.load(Ordering::Acquire) == 1);
        gate.store(true, Ordering::Release);
        let events = wait_for(&engine, |event| {
            matches!(event, TrackTileEngineEvent::SearchCompleted { .. })
        });
        let completed = events
            .iter()
            .filter_map(|event| match event {
                TrackTileEngineEvent::SearchCompleted {
                    generation,
                    results,
                } => Some((*generation, results)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].0, 2);
        assert_eq!(completed[0].1.query, "TP53");
        assert_eq!(
            completed[0]
                .1
                .hits
                .iter()
                .map(|hit| hit.track_id.get())
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn file_tracks_open_on_helpers_search_step_and_honour_contig_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genes.bed");
        std::fs::write(&path, "chr1\t100\t200\talpha\nchr1\t5000\t5100\tbeta\n").unwrap();
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_track(
            TrackSpec {
                id: TrackId::new(7),
                name: "genes".to_owned(),
                path,
                kind: TrackSpecKind::Bed,
            },
            BTreeMap::from([("1".to_owned(), "chr1".to_owned())]),
        );
        engine.add_track(
            TrackSpec {
                id: TrackId::new(8),
                name: "missing".to_owned(),
                path: directory.path().join("absent.bed"),
                kind: TrackSpecKind::Bed,
            },
            BTreeMap::new(),
        );
        // Views may be requested before the sources are open; "1" is the workspace contig.
        let mut aliased = view(7, 0.0, 1_000.0, 1.0);
        aliased.contig = "1".to_owned();
        let generation = engine.request_view(aliased).unwrap();
        let missing_generation = engine.request_view(view(8, 0.0, 1_000.0, 1.0)).unwrap();
        let mut pending = BTreeSet::from([generation, missing_generation]);
        // Track 8's open failure may arrive after its view settles: wait for both.
        let mut failure_reported = false;
        let events = wait_for(&engine, |event| {
            match event {
                TrackTileEngineEvent::ViewSettled { generation, .. } => {
                    pending.remove(generation);
                }
                TrackTileEngineEvent::TrackOpenFailed { track, .. }
                    if *track == TrackId::new(8) =>
                {
                    failure_reported = true;
                }
                _ => {}
            }
            pending.is_empty() && failure_reported
        });
        assert!(events.contains(&TrackTileEngineEvent::TrackOpened {
            track: TrackId::new(7)
        }));
        assert!(events.iter().any(|event| matches!(
            event,
            TrackTileEngineEvent::TrackOpenFailed { track, .. } if *track == TrackId::new(8)
        )));
        let snapshot = engine.view(1, TrackId::new(7)).unwrap();
        assert_eq!(snapshot.tiles[0].tile.contig, "chr1");
        assert_eq!(snapshot.tiles[0].scene.records.len(), 1);
        assert_eq!(
            snapshot.tiles[0].scene.records[0].name.as_deref(),
            Some("alpha")
        );
        assert_eq!(engine.view(1, TrackId::new(8)).unwrap().failed.len(), 1);

        engine.search(5, "bet".to_owned(), 10, false);
        let events = wait_for(&engine, |event| {
            matches!(event, TrackTileEngineEvent::SearchCompleted { .. })
        });
        let Some(TrackTileEngineEvent::SearchCompleted {
            generation: 5,
            results,
        }) = events.last()
        else {
            panic!("no search result");
        };
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].record.start, 5_000);

        engine.adjacent_feature(6, "1".to_owned(), 300, true);
        let events = wait_for(&engine, |event| {
            matches!(event, TrackTileEngineEvent::AdjacentFeatureCompleted { .. })
        });
        let Some(TrackTileEngineEvent::AdjacentFeatureCompleted { hit: Some(hit), .. }) =
            events.last()
        else {
            panic!("no adjacent feature");
        };
        assert_eq!((hit.track_id.get(), hit.record.start), (7, 5_000));
    }

    #[test]
    fn two_hundred_tracks_are_served_by_the_fixed_helper_pool() {
        let engine = engine(4, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        assert_eq!(engine.helpers.len(), 4);
        let mut pending = BTreeSet::new();
        for track in 1..=200_u64 {
            engine.add_source(
                TrackId::new(track),
                format!("t{track}"),
                Box::new(Probe::default()),
            );
            pending.insert(engine.request_view(view(track, 0.0, 2_000.0, 1.0)).unwrap());
        }
        // Coalesced notifications may be dropped under load; the snapshot is the authority.
        wait_until(|| {
            while engine.try_receive().unwrap().is_some() {}
            (1..=200_u64).all(|track| {
                engine
                    .view(1, TrackId::new(track))
                    .is_some_and(|snapshot| snapshot.is_settled() && snapshot.tiles.len() == 2)
            })
        });
    }

    #[test]
    fn the_helper_pool_size_is_configurable_beyond_the_default_limit() {
        let config = |helpers| TrackTileEngineConfig {
            helpers,
            ..TrackTileEngineConfig::default()
        };
        // `spawn` keeps its limit; the explicit limit raises it, within the absolute one.
        assert_eq!(TrackTileEngine::spawn(config(12)).helper_count(), 8);
        assert_eq!(
            TrackTileEngine::spawn_with_helper_limit(config(12), 16).helper_count(),
            12
        );
        assert_eq!(
            TrackTileEngine::spawn_with_helper_limit(config(12), 4).helper_count(),
            4
        );
        assert_eq!(
            TrackTileEngine::spawn_with_helper_limit(config(1_000), usize::MAX).helper_count(),
            MAXIMUM_CONFIGURABLE_TRACK_TILE_HELPERS
        );
        assert_eq!(
            [1, 2, 3, 4, 8, 12, 64].map(margin_read_limit),
            [1, 1, 2, 3, 6, 9, 48]
        );

        // Twelve tracks that are slow at the same time are all being read at once by twelve
        // helpers; the default pool of eight would leave four waiting.
        let gate = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(32);
        let engine = TrackTileEngine::spawn_with_helper_limit(
            TrackTileEngineConfig {
                helpers: 12,
                margin_tiles: 0,
                ..TrackTileEngineConfig::default()
            },
            12,
        );
        for track in 1..=12_u64 {
            engine.add_source(
                TrackId::new(track),
                format!("slow{track}"),
                Box::new(Probe {
                    gate: Some(Arc::clone(&gate)),
                    started: Some(started_tx.clone()),
                    ..Probe::default()
                }),
            );
            engine.request_view(view(track, 0.0, 500.0, 1.0)).unwrap();
        }
        for _ in 0..12 {
            started_rx.recv_timeout(WAIT).unwrap();
        }
        gate.store(true, Ordering::Release);
    }

    /// Loads tiles 1 and 2 of `track` and their margin (0 and 3) while `gate` is open.
    fn warm(engine: &TrackTileEngine, track: u64, probe: &Probe) {
        let generation = engine
            .request_view(view(track, 1_500.0, 3_000.0, 1.0))
            .unwrap();
        wait_for(engine, settled(track, generation));
        wait_until(|| probe.queries.lock().unwrap().len() == 4);
        wait_until(|| {
            engine
                .cache_stats(TrackId::new(track))
                .is_some_and(|stats| stats.tiles == 4)
        });
    }

    #[test]
    fn margin_reads_never_occupy_the_whole_pool() {
        // Two helpers, two tracks whose margin reads block and cannot be cancelled.
        let gate = Arc::new(AtomicBool::new(true));
        let (started_tx, started_rx) = mpsc::sync_channel(64);
        let engine = engine(2, 1, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let mut probes = Vec::new();
        for track in [1_u64, 2] {
            let probe = Probe {
                gate: Some(Arc::clone(&gate)),
                started: Some(started_tx.clone()),
                deaf: true,
                ..Probe::default()
            };
            engine.add_source(
                TrackId::new(track),
                format!("deaf{track}"),
                Box::new(probe.clone()),
            );
            warm(&engine, track, &probe);
            probes.push(probe);
        }
        while started_rx.try_recv().is_ok() {}
        gate.store(false, Ordering::Release);
        // Panning onto cached tiles: every visible tile is ready, and each track wants one
        // new margin tile (4). Only one of the two reads may start.
        for track in [1_u64, 2] {
            let generation = engine
                .request_view(view(track, 2_100.0, 3_600.0, 1.0))
                .unwrap();
            wait_for(&engine, settled(track, generation));
        }
        assert_eq!(started_rx.recv_timeout(WAIT).unwrap().index, 4);
        thread::sleep(Duration::from_millis(100));
        assert!(
            started_rx.try_recv().is_err(),
            "a second margin read took the last helper"
        );
        // So a third track's visible tiles are read at once, although the margin read in
        // flight can neither finish nor be cancelled.
        let fast = Probe::default();
        engine.add_source(TrackId::new(3), "fast".to_owned(), Box::new(fast));
        let generation = engine.request_view(view(3, 0.0, 2_000.0, 1.0)).unwrap();
        wait_for(&engine, settled(3, generation));
        assert_eq!(engine.view(1, TrackId::new(3)).unwrap().tiles.len(), 2);
        // The other track's margin tile is read once the first is done.
        gate.store(true, Ordering::Release);
        wait_until(|| {
            probes
                .iter()
                .all(|probe| probe.queries.lock().unwrap().len() == 5)
        });
    }

    #[test]
    fn a_visible_tile_without_a_free_helper_takes_one_from_another_tracks_margin_read() {
        let gate = Arc::new(AtomicBool::new(true));
        let (started_tx, started_rx) = mpsc::sync_channel(64);
        let slow = Probe {
            gate: Some(Arc::clone(&gate)),
            started: Some(started_tx),
            ..Probe::default()
        };
        // One helper: whatever it reads, nothing else is read.
        let engine = engine(1, 1, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "slow".to_owned(), Box::new(slow.clone()));
        warm(&engine, 1, &slow);
        while started_rx.try_recv().is_ok() {}
        gate.store(false, Ordering::Release);
        let panned = engine.request_view(view(1, 2_100.0, 3_600.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, panned));
        // The only helper is now inside the slow track's margin read of tile 4.
        assert_eq!(started_rx.recv_timeout(WAIT).unwrap().index, 4);

        let fast = Probe::default();
        engine.add_source(TrackId::new(2), "fast".to_owned(), Box::new(fast.clone()));
        let generation = engine.request_view(view(2, 0.0, 2_000.0, 1.0)).unwrap();
        // The margin read is cancelled for it, and the visible tiles are served.
        wait_for(&engine, settled(2, generation));
        assert_eq!(slow.cancelled.load(Ordering::Acquire), 1);
        assert_eq!(engine.view(1, TrackId::new(2)).unwrap().tiles.len(), 2);
        assert_eq!(engine.cache_stats(TrackId::new(1)).unwrap().cancelled, 1);

        // The margin tile was only postponed: it is read again and cached.
        gate.store(true, Ordering::Release);
        wait_until(|| engine.cache_stats(TrackId::new(1)).unwrap().tiles == 5);
        let indices = slow
            .queries
            .lock()
            .unwrap()
            .iter()
            .map(|key| key.index)
            .collect::<Vec<_>>();
        assert_eq!(indices[4..], [4, 4]);
    }

    #[test]
    fn a_track_gives_up_its_own_margin_read_for_a_visible_tile_of_another_panel() {
        let gate = Arc::new(AtomicBool::new(true));
        let (started_tx, started_rx) = mpsc::sync_channel(64);
        let probe = Probe {
            gate: Some(Arc::clone(&gate)),
            started: Some(started_tx),
            ..Probe::default()
        };
        let engine = engine(4, 1, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        warm(&engine, 1, &probe);
        while started_rx.try_recv().is_ok() {}
        gate.store(false, Ordering::Release);
        let panned = engine.request_view(view(1, 2_100.0, 3_600.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, panned));
        assert_eq!(started_rx.recv_timeout(WAIT).unwrap().index, 4);

        // A second panel shows tile 9 of the same track. Panel 1 still wants tile 4 as
        // margin, so the read is not abandoned as unwanted; it yields to the visible tile.
        let second = TrackTileView {
            panel: 2,
            ..view(1, 9_300.0, 9_800.0, 1.0)
        };
        let generation = engine.request_view(second).unwrap();
        assert_eq!(started_rx.recv_timeout(WAIT).unwrap().index, 9);
        assert_eq!(probe.cancelled.load(Ordering::Acquire), 1);
        gate.store(true, Ordering::Release);
        wait_for(&engine, |event| {
            *event
                == TrackTileEngineEvent::ViewSettled {
                    panel: 2,
                    track: TrackId::new(1),
                    generation,
                }
        });
        // Then the postponed margin tile, and the new panel's own margins.
        wait_until(|| {
            let queries = probe.queries.lock().unwrap();
            queries.iter().filter(|key| key.index == 4).count() == 2
        });
    }

    #[test]
    fn a_replaced_registration_does_not_inherit_the_old_source() {
        let gate = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(8);
        let old = Probe {
            gate: Some(Arc::clone(&gate)),
            started: Some(started_tx),
            ..Probe::default()
        };
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "old".to_owned(), Box::new(old.clone()));
        engine.request_view(view(1, 0.0, 500.0, 1.0)).unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        // Re-registered while the old source is lent to a helper.
        let new = Probe::default();
        engine.add_source(TrackId::new(1), "new".to_owned(), Box::new(new.clone()));
        wait_until(|| old.cancelled.load(Ordering::Acquire) == 1);
        let generation = engine.request_view(view(1, 0.0, 500.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, generation));
        assert_eq!(new.queries.lock().unwrap().len(), 1);
        // A later view is still read by the new source, not by the returned old one.
        let later = engine
            .request_view(view(1, 51_200.0, 51_700.0, 1.0))
            .unwrap();
        wait_for(&engine, settled(1, later));
        assert_eq!(new.queries.lock().unwrap().len(), 2);
        assert_eq!(old.queries.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_legacy_engine_shares_its_reader_instead_of_the_tile_engine_opening_a_second() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genes.bed");
        std::fs::write(&path, "chr1\t100\t200\talpha\nchr1\t5000\t5100\tbeta\n").unwrap();
        let specification = TrackSpec {
            id: TrackId::new(7),
            name: "genes".to_owned(),
            path: path.clone(),
            kind: TrackSpecKind::Bed,
        };
        let legacy = crate::TrackEngine::spawn_with_query_contig_aliases(
            vec![(
                specification,
                BTreeMap::from([("1".to_owned(), "chr1".to_owned())]),
            )],
            8,
        );
        let tiles = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        // The file is gone from disk before the tile engine hears of the track: whatever the
        // tile engine shows can only come from the reader the legacy engine opened.
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(
                Instant::now() < deadline,
                "the legacy engine never became ready"
            );
            if matches!(
                legacy.try_receive().unwrap(),
                Some(crate::TrackEngineEvent::Ready { opened: 1, .. })
            ) {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        std::fs::remove_file(&path).unwrap();

        legacy
            .try_share_tile_sources(vec![TrackId::new(7), TrackId::new(8)], &tiles)
            .unwrap();
        // Views may be requested at once, under the workspace contig name: the aliases of the
        // legacy track were carried over.
        let mut aliased = view(7, 0.0, 1_000.0, 1.0);
        aliased.contig = "1".to_owned();
        let generation = tiles.request_view(aliased).unwrap();
        let unknown = tiles.request_view(view(8, 0.0, 1_000.0, 1.0)).unwrap();
        let mut pending = BTreeSet::from([generation, unknown]);
        // Track 8's open failure may arrive after its view settles: wait for both.
        let mut failure_reported = false;
        let events = wait_for(&tiles, |event| {
            match event {
                TrackTileEngineEvent::ViewSettled { generation, .. } => {
                    pending.remove(generation);
                }
                TrackTileEngineEvent::TrackOpenFailed { track, .. }
                    if *track == TrackId::new(8) =>
                {
                    failure_reported = true;
                }
                _ => {}
            }
            pending.is_empty() && failure_reported
        });
        assert!(events.contains(&TrackTileEngineEvent::TrackOpened {
            track: TrackId::new(7)
        }));
        assert!(events.iter().any(|event| matches!(
            event,
            TrackTileEngineEvent::TrackOpenFailed { track, message }
                if *track == TrackId::new(8) && message.contains("not open")
        )));
        let snapshot = tiles.view(1, TrackId::new(7)).unwrap();
        assert_eq!(snapshot.tiles[0].tile.contig, "chr1");
        assert_eq!(
            snapshot.tiles[0].scene.records[0].name.as_deref(),
            Some("alpha")
        );
        assert_eq!(tiles.view(1, TrackId::new(8)).unwrap().failed.len(), 1);

        // Both engines keep working through the one reader: a tile-engine search and a
        // legacy search each find the feature.
        tiles.search(1, "bet".to_owned(), 10, false);
        let events = wait_for(&tiles, |event| {
            matches!(event, TrackTileEngineEvent::SearchCompleted { .. })
        });
        let Some(TrackTileEngineEvent::SearchCompleted { results, .. }) = events.last() else {
            panic!("expected the search result");
        };
        assert_eq!(results.hits.len(), 1);
        legacy.try_search(1, "alph".to_owned(), 10, false).unwrap();
        let deadline = Instant::now() + WAIT;
        let hits = loop {
            assert!(
                Instant::now() < deadline,
                "the legacy search never completed"
            );
            match legacy.try_receive().unwrap() {
                Some(crate::TrackEngineEvent::SearchCompleted { results, .. }) => {
                    break results.hits;
                }
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(2)),
            }
        };
        assert_eq!(hits.len(), 1);

        // Removing the track from the legacy engine leaves the tile engine's handle working.
        legacy.try_remove_many(vec![TrackId::new(7)]).unwrap();
        let later = tiles.request_view(view(7, 4_500.0, 5_200.0, 1.0)).unwrap();
        // (workspace contig "chr1" is the source's own spelling)
        wait_for(&tiles, settled(7, later));
        let snapshot = tiles.view(1, TrackId::new(7)).unwrap();
        assert!(snapshot.failed.is_empty());
        assert!(snapshot.tiles.iter().any(|tile| {
            tile.scene
                .records
                .iter()
                .any(|record| record.name.as_deref() == Some("beta"))
        }));
    }

    /// A source of stacked features whose complete records are only available by lookup.
    #[derive(Clone, Default)]
    struct Stacked {
        rows: u32,
        lookups: Arc<AtomicUsize>,
        cancelled: Arc<AtomicUsize>,
        /// When set, a lookup blocks until released or cancelled.
        gate: Option<Arc<AtomicBool>>,
        fail_lookups: bool,
    }

    impl Stacked {
        fn record(row: u32) -> FeatureRecord {
            FeatureRecord {
                contig: "chr1".to_owned(),
                start: 100,
                end: 600,
                name: Some(format!("gene{row}")),
                feature_type: Some("mRNA".to_owned()),
                score: Some(1.5),
                strand: Strand::Forward,
                blocks: vec![(100, 200), (500, 600)],
                coding_interval: Some((150, 550)),
                peak: None,
                attributes: vec![("ID".to_owned(), format!("tx{row}"))],
            }
        }
    }

    impl TrackTileSource for Stacked {
        fn query_tile(
            &mut self,
            request: &TrackTileRequest,
            _cancel: &TrackCancellation,
        ) -> Result<TrackTile, TrackTileError> {
            let features = (0..self.rows)
                .map(|row| {
                    let record = Self::record(row);
                    TileFeature {
                        id: TrackRecordId::for_feature(&record),
                        start: record.start,
                        end: record.end,
                        strand: record.strand,
                        blocks: Vec::new(),
                        name: record.name.as_deref().map(Arc::from),
                        score: record.score,
                        color: None,
                        row_hint: Some(row),
                        labelable: true,
                    }
                })
                .collect();
            Ok(TrackTile {
                contig: request.contig.clone(),
                key: request.tile,
                lod: TrackTileLod::Detail,
                payload: TrackTilePayload::Features(FeatureTile {
                    features,
                    rows: FeatureRowAxis::Packed,
                }),
                provenance: TrackTileProvenance::default(),
            })
        }

        fn lookup_record(
            &mut self,
            query: &TrackRecordQuery,
            cancel: &TrackCancellation,
        ) -> Result<Option<TrackRecord>, TrackTileError> {
            self.lookups.fetch_add(1, Ordering::AcqRel);
            if let Some(gate) = &self.gate {
                let deadline = Instant::now() + WAIT;
                while !gate.load(Ordering::Acquire) {
                    if cancel.is_cancelled() {
                        self.cancelled.fetch_add(1, Ordering::AcqRel);
                        return Err(TrackTileError::Cancelled);
                    }
                    assert!(Instant::now() < deadline, "gated lookup was never released");
                    thread::sleep(Duration::from_millis(1));
                }
            }
            if self.fail_lookups {
                return Err(TrackTileError::Source("unreadable".to_owned()));
            }
            assert_eq!((query.start, query.end), (100, 600));
            Ok((0..self.rows)
                .map(Self::record)
                .find(|record| TrackRecordId::for_feature(record) == query.id)
                .map(TrackRecord::Feature))
        }
    }

    fn stacked_id(row: u32) -> TrackRecordId {
        TrackRecordId::for_feature(&Stacked::record(row))
    }

    #[test]
    fn the_views_row_cap_bounds_the_scene_and_the_snapshot_reports_what_it_hid() {
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let source = Stacked {
            rows: 20,
            ..Stacked::default()
        };
        engine.add_source(TrackId::new(1), "stacked".to_owned(), Box::new(source));
        // The default cap is today's twelve rows.
        let generation = engine.request_view(view(1, 0.0, 1_000.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, generation));
        let snapshot = engine.view(1, TrackId::new(1)).unwrap();
        let scene = &snapshot.tiles[0].scene;
        assert_eq!((scene.row_count, scene.row_cap), (20, Some(12)));
        assert_eq!((scene.hidden_rows, scene.hidden_features), (8, 8));
        assert_eq!(scene.instances.len(), 12);
        assert_eq!(scene.records.len(), 20);
        let overflow = snapshot.overflow();
        assert_eq!((overflow.hidden_rows, overflow.hidden_features), (8, 8));

        // A larger cap is another tile (the cap is part of the settings digest) with every
        // row drawn and nothing reported hidden.
        let mut roomy = view(1, 0.0, 1_000.0, 1.0);
        roomy.settings = TrackTileSettings::default().with_maximum_feature_rows(32);
        let roomy_digest = roomy.settings.digest();
        assert_ne!(roomy_digest, TrackTileSettings::default().digest());
        let generation = engine.request_view(roomy).unwrap();
        wait_for(&engine, settled(1, generation));
        let snapshot = engine.view(1, TrackId::new(1)).unwrap();
        let scene = &snapshot.tiles[0].scene;
        assert_eq!((scene.row_count, scene.row_cap), (20, Some(32)));
        assert_eq!(scene.instances.len(), 20);
        assert!(snapshot.overflow().is_empty());
        assert_eq!(snapshot.settings_digest, roomy_digest);

        // A tighter cap hides more, and no cap hides nothing.
        for (cap, hidden) in [(3_u32, 17_u32), (u32::MAX, 0)] {
            let mut capped = view(1, 0.0, 1_000.0, 1.0);
            capped.settings = TrackTileSettings::default().with_maximum_feature_rows(cap);
            let generation = engine.request_view(capped).unwrap();
            wait_for(&engine, settled(1, generation));
            let snapshot = engine.view(1, TrackId::new(1)).unwrap();
            assert_eq!(snapshot.overflow().hidden_rows, hidden);
            assert_eq!(snapshot.overflow().hidden_features, hidden);
            assert_eq!(
                snapshot.tiles[0].scene.instances.len(),
                (20 - hidden) as usize
            );
        }
    }

    #[test]
    fn a_snapshot_names_its_contig_so_tiles_of_two_contigs_are_told_apart() {
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let probe = Probe::default();
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe));
        engine.set_contig_aliases(
            TrackId::new(1),
            BTreeMap::from([("2".to_owned(), "chr2".to_owned())]),
        );
        let mut first = view(1, 0.0, 1_000.0, 1.0);
        first.panel = 1;
        let mut second = view(1, 0.0, 1_000.0, 1.0);
        second.panel = 2;
        second.contig = "2".to_owned();
        let first_generation = engine.request_view(first).unwrap();
        let second_generation = engine.request_view(second).unwrap();
        let mut pending = BTreeSet::from([first_generation, second_generation]);
        wait_for(&engine, |event| {
            if let TrackTileEngineEvent::ViewSettled { generation, .. } = event {
                pending.remove(generation);
            }
            pending.is_empty()
        });
        let first = engine.view(1, TrackId::new(1)).unwrap();
        let second = engine.view(2, TrackId::new(1)).unwrap();
        // The same tile key and settings on two contigs: only the contig tells them apart.
        assert_eq!(first.tiles[0].tile.key, second.tiles[0].tile.key);
        assert_eq!(
            first.tiles[0].settings_digest,
            second.tiles[0].settings_digest
        );
        assert_eq!(first.settings_digest, first.tiles[0].settings_digest);
        assert_eq!((first.contig(), first.source_contig()), ("chr1", "chr1"));
        assert_eq!((second.contig(), second.source_contig()), ("2", "chr2"));
        assert_eq!(first.tiles[0].contig(), first.source_contig());
        assert_eq!(second.tiles[0].contig(), second.source_contig());
        assert_ne!(first.tiles[0].contig(), second.tiles[0].contig());
        assert!(!Arc::ptr_eq(&first.tiles[0], &second.tiles[0]));
        // A default snapshot names no contig.
        assert_eq!(TrackTileViewSnapshot::default().contig(), "");
    }

    #[test]
    fn a_record_lookup_is_one_bounded_read_newest_wins_and_is_cancellable() {
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let gate = Arc::new(AtomicBool::new(false));
        let source = Stacked {
            rows: 3,
            gate: Some(Arc::clone(&gate)),
            ..Stacked::default()
        };
        engine.add_source(
            TrackId::new(1),
            "stacked".to_owned(),
            Box::new(source.clone()),
        );
        let track = TrackId::new(1);
        // Nothing is cached yet: the identity's interval is unknown and nothing is read.
        assert_eq!(
            engine.lookup_record(track, "chr1", stacked_id(0)),
            TrackRecordLookup::NotFound
        );
        let generation = engine.request_view(view(1, 0.0, 1_000.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, generation));
        // An identity no tile shows, an unknown track and another contig read nothing either.
        for outcome in [
            engine.lookup_record(track, "chr1", TrackRecordId(7)),
            engine.lookup_record(TrackId::new(9), "chr1", stacked_id(0)),
            engine.lookup_record(track, "chr2", stacked_id(0)),
        ] {
            assert_eq!(outcome, TrackRecordLookup::NotFound);
        }
        assert_eq!(source.lookups.load(Ordering::Acquire), 0);

        // The source kept no records with the tile, so the first lookup schedules a read.
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(0))
                .is_pending()
        );
        wait_until(|| source.lookups.load(Ordering::Acquire) == 1);
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(0))
                .is_pending()
        );
        // Asking for another record cancels the running read; only the newest is answered.
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(1))
                .is_pending()
        );
        wait_until(|| source.cancelled.load(Ordering::Acquire) == 1);
        wait_until(|| source.lookups.load(Ordering::Acquire) == 2);
        gate.store(true, Ordering::Release);
        let outcome = engine.lookup_record_blocking(track, "chr1", stacked_id(1), WAIT);
        assert_eq!(
            outcome.record().and_then(TrackRecord::as_feature),
            Some(&Stacked::record(1))
        );
        // Resolved once, it is answered from memory.
        assert_eq!(engine.lookup_record(track, "chr1", stacked_id(1)), outcome);
        assert_eq!(source.lookups.load(Ordering::Acquire), 2);
        // The cancelled lookup left nothing behind; asking again reads again.
        let outcome = engine.lookup_record_blocking(track, "chr1", stacked_id(0), WAIT);
        assert_eq!(
            outcome.record().and_then(TrackRecord::as_feature),
            Some(&Stacked::record(0))
        );
        assert_eq!(source.lookups.load(Ordering::Acquire), 3);

        // An explicit cancellation stops a running read and leaves the lookup unanswered.
        gate.store(false, Ordering::Release);
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(2))
                .is_pending()
        );
        wait_until(|| source.lookups.load(Ordering::Acquire) == 4);
        engine.cancel_record_lookup(track);
        wait_until(|| source.cancelled.load(Ordering::Acquire) == 2);
        // A wait that runs out of time says so and does not block.
        assert!(
            engine
                .lookup_record_blocking(track, "chr1", stacked_id(2), Duration::from_millis(30))
                .is_pending()
        );
        // Tiles keep being served while a lookup waits on the source: dropping the engine
        // with the lookup in flight cancels it and returns.
        drop(engine);
        wait_until(|| source.cancelled.load(Ordering::Acquire) == 3);
    }

    #[test]
    fn a_failed_record_lookup_is_reported_once_and_retried_on_the_next_request() {
        let engine = engine(1, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let source = Stacked {
            rows: 2,
            fail_lookups: true,
            ..Stacked::default()
        };
        engine.add_source(
            TrackId::new(1),
            "stacked".to_owned(),
            Box::new(source.clone()),
        );
        let generation = engine.request_view(view(1, 0.0, 1_000.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, generation));
        let track = TrackId::new(1);
        let outcome = engine.lookup_record_blocking(track, "chr1", stacked_id(0), WAIT);
        assert_eq!(
            outcome,
            TrackRecordLookup::Failed("stacked: unreadable".to_owned())
        );
        assert_eq!(source.lookups.load(Ordering::Acquire), 1);
        // The failure was handed over; the next request is a fresh attempt.
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(0))
                .is_pending()
        );
        wait_until(|| source.lookups.load(Ordering::Acquire) == 2);
    }

    #[test]
    fn asking_again_for_a_cancelled_lookup_reads_again_instead_of_waiting_on_the_dead_read() {
        let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        let gate = Arc::new(AtomicBool::new(false));
        let source = Stacked {
            rows: 2,
            gate: Some(Arc::clone(&gate)),
            ..Stacked::default()
        };
        engine.add_source(
            TrackId::new(1),
            "stacked".to_owned(),
            Box::new(source.clone()),
        );
        let generation = engine.request_view(view(1, 0.0, 1_000.0, 1.0)).unwrap();
        wait_for(&engine, settled(1, generation));
        let track = TrackId::new(1);
        // A read starts and is cancelled while it is still blocked on the source.
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(1))
                .is_pending()
        );
        wait_until(|| source.lookups.load(Ordering::Acquire) == 1);
        engine.cancel_record_lookup(track);
        // The same record is asked for again before the cancelled read has returned: that
        // is a new lookup, not the dead one.
        assert!(
            engine
                .lookup_record(track, "chr1", stacked_id(1))
                .is_pending()
        );
        wait_until(|| source.cancelled.load(Ordering::Acquire) == 1);
        wait_until(|| source.lookups.load(Ordering::Acquire) == 2);
        gate.store(true, Ordering::Release);
        let outcome = engine.lookup_record_blocking(track, "chr1", stacked_id(1), WAIT);
        assert_eq!(
            outcome.record().and_then(TrackRecord::as_feature),
            Some(&Stacked::record(1))
        );
        // The blocking form of a request on another contig spelling of the same track does
        // not wait on this track's lookup: nothing shows the record there.
        assert_eq!(
            engine.lookup_record_blocking(track, "chr9", stacked_id(1), WAIT),
            TrackRecordLookup::NotFound
        );
        assert_eq!(source.lookups.load(Ordering::Acquire), 2);
    }

    #[test]
    fn a_view_requested_after_the_track_failed_to_open_fails_instead_of_loading_for_ever() {
        let directory = tempfile::tempdir().unwrap();
        let engine = engine(1, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_track(
            TrackSpec {
                id: TrackId::new(8),
                name: "missing".to_owned(),
                path: directory.path().join("absent.bed"),
                kind: TrackSpecKind::Bed,
            },
            BTreeMap::new(),
        );
        wait_for(&engine, |event| {
            matches!(event, TrackTileEngineEvent::TrackOpenFailed { .. })
        });
        // The failure is already known when the view arrives.
        let generation = engine.request_view(view(8, 0.0, 1_000.0, 1.0)).unwrap();
        let events = wait_for(&engine, settled(8, generation));
        assert!(events.iter().any(|event| matches!(
            event,
            TrackTileEngineEvent::TileFailed { message, .. } if message.starts_with("missing: ")
        )));
        let snapshot = engine.view(1, TrackId::new(8)).unwrap();
        assert!(snapshot.is_settled());
        assert_eq!(snapshot.failed.len(), 1);
    }

    /// A built-in source with the records it would hand over with each tile withheld, so
    /// every lookup takes the re-read path.
    struct WithoutRetention {
        source: Box<dyn TrackTileSource>,
        lookups: Arc<AtomicUsize>,
    }

    impl TrackTileSource for WithoutRetention {
        fn query_tile(
            &mut self,
            request: &TrackTileRequest,
            cancel: &TrackCancellation,
        ) -> Result<TrackTile, TrackTileError> {
            let tile = self.source.query_tile(request, cancel);
            drop(self.source.take_retained_records());
            tile
        }

        fn lookup_record(
            &mut self,
            query: &TrackRecordQuery,
            cancel: &TrackCancellation,
        ) -> Result<Option<TrackRecord>, TrackTileError> {
            self.lookups.fetch_add(1, Ordering::AcqRel);
            self.source.lookup_record(query, cancel)
        }
    }

    const BED12: &str = "\
chr1\t1000\t5000\ttxA\t960\t+\t1200\t4500\t255,0,0\t3\t500,400,1000\t0,1500,3000\n\
chr1\t2000\t9000\ttxB\t10\t-\t2000\t2000\t0\t2\t300,700\t0,6300\n\
chr1\t12000\t12500\tsingle\t0\t.\t12000\t12500\t0\t1\t500\t0\n";

    const GFF3: &str = "\
##gff-version 3\n\
chr1\tsrc\tgene\t1001\t9000\t.\t+\t.\tID=gene1;Name=ALPHA;gene_biotype=protein_coding\n\
chr1\tsrc\tmRNA\t1001\t9000\t.\t+\t.\tID=tx1;Parent=gene1;Name=ALPHA-201;tag=basic\n\
chr1\tsrc\texon\t1001\t1500\t.\t+\t.\tID=ex1;Parent=tx1\n\
chr1\tsrc\texon\t3001\t3600\t.\t+\t.\tID=ex2;Parent=tx1\n\
chr1\tsrc\texon\t8001\t9000\t.\t+\t.\tID=ex3;Parent=tx1\n\
chr1\tsrc\tCDS\t1201\t1500\t.\t+\t0\tID=cds1;Parent=tx1\n\
chr1\tsrc\tCDS\t3001\t3600\t.\t+\t0\tID=cds1;Parent=tx1\n\
chr1\tsrc\tCDS\t8001\t8400\t.\t+\t0\tID=cds1;Parent=tx1\n\
chr1\tsrc\tgene\t20001\t21000\t5.5\t-\t.\tID=gene2;Name=BETA;note=a%20note\n";

    const GTF: &str = "\
chr1\tsrc\tgene\t1001\t9000\t.\t+\t.\tgene_id \"G1\"; gene_name \"ALPHA\"; gene_biotype \"protein_coding\";\n\
chr1\tsrc\ttranscript\t1001\t9000\t.\t+\t.\tgene_id \"G1\"; transcript_id \"T1\"; gene_name \"ALPHA\"; transcript_name \"ALPHA-201\";\n\
chr1\tsrc\texon\t1001\t1500\t.\t+\t.\tgene_id \"G1\"; transcript_id \"T1\"; exon_number \"1\";\n\
chr1\tsrc\texon\t3001\t3600\t.\t+\t.\tgene_id \"G1\"; transcript_id \"T1\"; exon_number \"2\";\n\
chr1\tsrc\texon\t8001\t9000\t.\t+\t.\tgene_id \"G1\"; transcript_id \"T1\"; exon_number \"3\";\n\
chr1\tsrc\tCDS\t1201\t1500\t.\t+\t0\tgene_id \"G1\"; transcript_id \"T1\";\n\
chr1\tsrc\tCDS\t3001\t3600\t.\t+\t0\tgene_id \"G1\"; transcript_id \"T1\";\n\
chr1\tsrc\tCDS\t8001\t8400\t.\t+\t0\tgene_id \"G1\"; transcript_id \"T1\";\n\
chr1\tsrc\tgene\t20001\t21000\t.\t-\t.\tgene_id \"G2\"; gene_name \"BETA\";\n\
chr1\tsrc\ttranscript\t20001\t21000\t.\t-\t.\tgene_id \"G2\"; transcript_id \"T2\";\n\
chr1\tsrc\texon\t20001\t21000\t.\t-\t.\tgene_id \"G2\"; transcript_id \"T2\";\n";

    #[test]
    fn a_looked_up_record_equals_the_legacy_querys_record_for_bed12_gff3_and_gtf() {
        let directory = tempfile::tempdir().unwrap();
        for (name, text, kind) in [
            ("genes.bed", BED12, TrackSpecKind::Bed),
            ("genes.gff3", GFF3, TrackSpecKind::Gff3),
            ("genes.gtf", GTF, TrackSpecKind::Gtf),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, text).unwrap();
            let specification = |id: u64| TrackSpec {
                id: TrackId::new(id),
                name: name.to_owned(),
                path: path.clone(),
                kind,
            };
            // What the legacy inspector is given: the legacy query's own records.
            let legacy = crate::track_engine::legacy_feature_records(
                &specification(1),
                "chr1",
                0,
                1_000_000,
            );
            // BED12 has three records; GFF3 and GTF one assembled transcript model for ALPHA
            // (its gene line not drawn) and one more feature for BETA.
            assert_eq!(
                legacy.len(),
                if kind == TrackSpecKind::Bed { 3 } else { 2 },
                "{name}: {legacy:?}"
            );
            // The fixtures exercise what a tile does not carry: the complete block list and
            // coding interval (of BED12 and of the assembled GFF3 and GTF transcripts), and
            // the type and attribute list of GFF3 and GTF records.
            if kind != TrackSpecKind::Bed {
                let model = legacy
                    .iter()
                    .find(|record| record.display_label() == Some("ALPHA"))
                    .unwrap_or_else(|| panic!("{name}: {legacy:#?}"));
                assert_eq!(model.feature_type.as_deref(), Some("transcript"), "{name}");
                assert_eq!((model.start, model.end), (1_000, 9_000), "{name}");
                assert_eq!(
                    model.blocks,
                    [(1_000, 1_500), (3_000, 3_600), (8_000, 9_000)],
                    "{name}"
                );
                assert_eq!(model.coding_interval, Some((1_200, 8_400)), "{name}");
                assert!(
                    !legacy
                        .iter()
                        .any(|record| record.feature_type.as_deref() == Some("gene")
                            && record.display_label() == Some("ALPHA")),
                    "{name}: the gene line of a drawn transcript is not drawn"
                );
            }
            if kind == TrackSpecKind::Bed {
                assert!(
                    legacy.iter().any(|record| record.blocks.len() >= 3),
                    "{name}"
                );
                assert!(
                    legacy.iter().any(|record| record.coding_interval.is_some()),
                    "{name}"
                );
                assert!(legacy.iter().any(|record| record.score.is_some()), "{name}");
            } else {
                assert!(
                    legacy.iter().all(|record| record.feature_type.is_some()),
                    "{name}"
                );
                assert!(
                    legacy.iter().all(|record| record.attributes.len() >= 2),
                    "{name}"
                );
                assert!(
                    legacy.iter().any(|record| record
                        .attributes
                        .iter()
                        .any(|(key, _)| key == "transcript_id" || key == "Parent")),
                    "{name}"
                );
            }
            let legacy_with = |id: TrackRecordId| {
                legacy
                    .iter()
                    .filter(|record| TrackRecordId::for_feature(record) == id)
                    .collect::<Vec<_>>()
            };

            let engine = engine(2, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
            // Track 1 is the built-in source as the desktop opens it; track 2 is the same
            // source with nothing retained, so its lookups re-read the file.
            engine.add_track(specification(1), BTreeMap::new());
            let rereads = Arc::new(AtomicUsize::new(0));
            engine.add_source(
                TrackId::new(2),
                name.to_owned(),
                Box::new(WithoutRetention {
                    source: crate::track_engine::open_tile_source(
                        &specification(2),
                        &mut HashMap::new(),
                        &HttpRangeConfig::default(),
                    )
                    .unwrap(),
                    lookups: Arc::clone(&rereads),
                }),
            );
            // 32 bases per point: one level-5 tile holds the whole fixture.
            let mut pending = BTreeSet::from([
                engine.request_view(view(1, 0.0, 30_000.0, 32.0)).unwrap(),
                engine.request_view(view(2, 0.0, 30_000.0, 32.0)).unwrap(),
            ]);
            wait_for(&engine, |event| {
                if let TrackTileEngineEvent::ViewSettled { generation, .. } = event {
                    pending.remove(generation);
                }
                pending.is_empty()
            });
            let mut resolved = 0;
            for track in [1, 2] {
                let track = TrackId::new(track);
                let snapshot = engine.view(1, track).unwrap();
                assert!(snapshot.failed.is_empty(), "{name}: {:?}", snapshot.failed);
                let ids = snapshot
                    .tiles
                    .iter()
                    .flat_map(|tile| tile.scene.records.iter().map(|record| record.id))
                    .collect::<BTreeSet<_>>();
                // Every legacy record is shown by the tiles.
                assert_eq!(
                    ids,
                    legacy
                        .iter()
                        .map(TrackRecordId::for_feature)
                        .collect::<BTreeSet<_>>(),
                    "{name}"
                );
                for id in ids {
                    let outcome = if track.get() == 1 {
                        // Retained with the tile: answered at once, with no read.
                        engine.lookup_record(track, "chr1", id)
                    } else {
                        engine.lookup_record_blocking(track, "chr1", id, WAIT)
                    };
                    let TrackRecordLookup::Ready(record) = &outcome else {
                        panic!("{name}: {id:?} resolved to {outcome:?}");
                    };
                    let record = record.as_feature().unwrap();
                    let expected = legacy_with(id);
                    assert_eq!(
                        expected.len(),
                        1,
                        "{name}: {id:?} is ambiguous in the fixture"
                    );
                    assert_eq!(record, expected[0], "{name}");
                    assert_eq!(TrackRecordId::for_feature(record), id);
                    resolved += 1;
                }
            }
            assert_eq!(resolved, legacy.len() * 2);
            // Only the track without retained records read its source for lookups, once per
            // record.
            assert_eq!(rereads.load(Ordering::Acquire), legacy.len(), "{name}");
        }
    }

    #[test]
    fn dropping_the_engine_cancels_in_flight_reads_and_does_not_block() {
        let gate = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::sync_channel(8);
        let probe = Probe {
            gate: Some(gate),
            started: Some(started_tx),
            ..Probe::default()
        };
        let engine = engine(1, 0, DEFAULT_TRACK_TILE_CACHE_BYTES);
        engine.add_source(TrackId::new(1), "probe".to_owned(), Box::new(probe.clone()));
        engine.request_view(view(1, 0.0, 500.0, 1.0)).unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        let started = Instant::now();
        drop(engine);
        assert!(started.elapsed() < Duration::from_secs(2));
        wait_until(|| probe.cancelled.load(Ordering::Acquire) == 1);
    }
}
