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
//!   parts, visible tiles, margin tiles; round-robin over tracks within a class.
//!
//! The engine never blocks its caller: every method takes one short lock and returns.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use rgv_core::TrackId;
use rgv_hts::{HttpRangeConfig, MAX_FEATURE_SEARCH_RESULTS};
use rgv_model::{
    FeatureRecord, TrackCancellation, TrackTile, TrackTileError, TrackTileKey, TrackTileRequest,
    TrackTileScene, TrackTileSettings,
};

use crate::{
    EngineQueueError, TrackSearchHit, TrackSearchResults, TrackSourceDiagnostics, TrackSpec,
    TrackSpecKind, TrackTileLayoutRegistry, TrackTileSearch, TrackTileSearchHits, TrackTileSource,
    mailbox::{
        CoalescingReceiver, CoalescingSender, bounded_coalescing_to_ui, panic_message, reap_worker,
    },
    track_engine::{TdfSourceCache, open_tile_source},
};

/// Default byte budget of one track's tile cache.
pub const DEFAULT_TRACK_TILE_CACHE_BYTES: usize = 64 << 20;
/// Most helper threads the engine starts, whatever the number of tracks.
pub const MAXIMUM_TRACK_TILE_HELPERS: usize = 8;

#[derive(Clone)]
pub struct TrackTileEngineConfig {
    /// Helper threads; clamped to `1..=MAXIMUM_TRACK_TILE_HELPERS`.
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
}

impl TrackTileViewSnapshot {
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

struct CacheEntry {
    tile: Arc<LaidOutTrackTile>,
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
        budget: usize,
        wanted: &dyn Fn(&CacheKey) -> bool,
    ) -> u64 {
        self.tick += 1;
        let bytes = tile.resident_bytes();
        if let Some(previous) = self.entries.insert(
            key,
            CacheEntry {
                tile,
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
}

enum SourceSlot {
    Unopened(TrackSpec),
    Idle(Box<dyn TrackTileSource>),
    /// Lent to a helper, or being opened by one.
    Busy,
    Failed,
}

struct InFlight {
    cancel: Arc<AtomicBool>,
    /// The tile being read, when the operation is a tile read.
    tile: Option<CacheKey>,
}

struct TrackState {
    name: String,
    aliases: BTreeMap<String, String>,
    source: SourceSlot,
    in_flight: Option<InFlight>,
    views: BTreeMap<u64, View>,
    cache: TileCache,
    stats: TrackTileCacheStats,
    diagnostics: Option<TrackSourceDiagnostics>,
}

impl TrackState {
    fn wanted(&self, key: &CacheKey) -> bool {
        self.views.values().any(|view| view.wants(key))
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
        });
        let helpers = (0..config.helpers.clamp(1, MAXIMUM_TRACK_TILE_HELPERS))
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
            .collect();
        Self {
            shared,
            output,
            helpers,
        }
    }

    /// Registers a file-backed track. It is opened on a helper; `TrackOpened` or
    /// `TrackOpenFailed` follows. Views may be requested immediately.
    pub fn add_track(&self, specification: TrackSpec, aliases: BTreeMap<String, String>) {
        let mut state = self.shared.lock();
        state.tracks.insert(
            specification.id,
            TrackState {
                name: specification.name.clone(),
                aliases,
                source: SourceSlot::Unopened(specification),
                in_flight: None,
                views: BTreeMap::new(),
                cache: TileCache::default(),
                stats: TrackTileCacheStats::default(),
                diagnostics: None,
            },
        );
        drop(state);
        self.shared.work.notify_all();
    }

    /// Registers a track backed by any [`TrackTileSource`]: the plugin entry point.
    pub fn add_source(&self, id: TrackId, name: String, source: Box<dyn TrackTileSource>) {
        let mut state = self.shared.lock();
        state.tracks.insert(
            id,
            TrackState {
                name,
                aliases: BTreeMap::new(),
                source: SourceSlot::Idle(source),
                in_flight: None,
                views: BTreeMap::new(),
                cache: TileCache::default(),
                stats: TrackTileCacheStats::default(),
                diagnostics: None,
            },
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
            if let Some(track) = state.tracks.remove(id)
                && let Some(in_flight) = &track.in_flight
            {
                in_flight.cancel.store(true, Ordering::Release);
            }
        }
        drop(state);
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
        track.views.insert(view.panel, new_view);
        // A read nobody wants any more stops now, not when its transfer happens to finish.
        if let Some(in_flight) = &track.in_flight
            && let Some(tile) = &in_flight.tile
            && !track.wanted(tile)
        {
            in_flight.cancel.store(true, Ordering::Release);
        }
        settle_views(view.track, track, &mut events);
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
        }
        for fan_out in [&state.search, &state.adjacent].into_iter().flatten() {
            fan_out.cancel.store(true, Ordering::Release);
        }
        drop(state);
        self.output.close();
        self.shared.work.notify_all();
        for helper in self.helpers.drain(..) {
            drop(reap_worker(
                helper,
                "rgv-track-tile-reaper",
                "track tile helper",
            ));
        }
    }
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
}

struct Job {
    track: TrackId,
    cancel: Arc<AtomicBool>,
    work: JobWork,
}

enum JobOutcome {
    Opened(Result<Box<dyn TrackTileSource>, String>),
    Tile {
        source: Option<Box<dyn TrackTileSource>>,
        key: CacheKey,
        result: Result<Arc<LaidOutTrackTile>, TrackTileError>,
    },
    FanOut {
        source: Option<Box<dyn TrackTileSource>>,
        generation: u64,
        is_search: bool,
        result: Result<FanOutPart, TrackTileError>,
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
            state.cursor = Some(*id);
            return Some(Job {
                track: *id,
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
    // 3. Visible tiles of every track, then 4. margins.
    for visible in [true, false] {
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
            state.cursor = Some(*id);
            return Some(Job {
                track: *id,
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
                cancel.check()?;
                let scene = shared.layouts.lay_out(&tile)?;
                Ok(Arc::new(LaidOutTrackTile {
                    tile,
                    scene,
                    settings_digest: key.digest,
                }))
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
    outcome: JobOutcome,
    events: &mut Vec<TrackTileEngineEvent>,
) {
    // The fan-out bookkeeping happens whether or not the track still exists.
    let (returned, tile_outcome) = match outcome {
        JobOutcome::Opened(result) => {
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
                Err(message) => {
                    track.source = SourceSlot::Failed;
                    let message = format!("{}: {message}", track.name);
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
        JobOutcome::Tile {
            source,
            key,
            result,
        } => (source, Some((key, result))),
    };
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
        Ok(tile) => {
            track.stats.completed += 1;
            let TrackState {
                cache,
                views,
                stats,
                ..
            } = track;
            // A complete tile is cached even if its view has moved on: it is viewport
            // independent and the user may pan back.
            stats.evictions +=
                cache.insert(key.clone(), tile, shared.cache_bytes_per_track, &|key| {
                    views.values().any(|view| view.wants(key))
                });
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
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            complete_job(shared, &mut state, job.track, outcome, &mut events);
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
        TrackRecordId, TrackTileLod, TrackTilePayload, TrackTileProvenance,
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
                    if cancel.is_cancelled() {
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
        let events = wait_for(&engine, |event| {
            if let TrackTileEngineEvent::ViewSettled { generation, .. } = event {
                pending.remove(generation);
            }
            pending.is_empty()
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
