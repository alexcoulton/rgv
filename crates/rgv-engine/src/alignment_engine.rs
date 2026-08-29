use std::{
    collections::HashMap,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Weak},
    thread::{self, JoinHandle},
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use rgv_core::{
    GenerationClock, GenomicInterval, PanelGeneration, PanelId, ReferenceDictionary,
    ViewportGeneration,
};
use rgv_hts::{
    AlignmentDerivationMetrics, AlignmentDerivationPhase, AlignmentError, AlignmentIndexOverview,
    AlignmentOpenOptions, AlignmentOverlapIndex, AlignmentQuery, AlignmentSource,
    AlignmentSummaryIndex, FastaOpenOptions, FastaReference, ProductionAlignmentSource,
    QueryDetail, QueryOptions, RemoteCancellation, RemoteError, RemoteIoStats,
    RemoteTransferBudget, derive_alignment_query_with_summary_index_and_phase,
};
use thiserror::Error;

use crate::{
    CacheStats, WeightedLruCache,
    mailbox::{CoalescingReceiver, CoalescingSender, bounded_coalescing},
};

const ALIGNMENT_CACHE_BYTES: usize = 320 * 1024 * 1024;
const REMOTE_SPECULATIVE_DECODED_BYTES: usize = 32 * 1024 * 1024;
const REMOTE_SPECULATIVE_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
const REMOTE_SPECULATIVE_REQUESTS: u64 = 64;
const MINIMUM_SUMMARY_INDEX_ALLOWANCE: usize = 16 * 1024 * 1024;
pub const ALIGNMENT_LAYOUT_TILE_BASES: u64 = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentBrowsingPolicy {
    pub buffer_percent: u16,
}

impl Default for AlignmentBrowsingPolicy {
    fn default() -> Self {
        Self {
            buffer_percent: rgv_core::DEFAULT_READ_BROWSING_BUFFER_PERCENT,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentCacheOutcome {
    Exact,
    ResidentExactReturn,
    ResidentContainment,
    ResidentExtension,
    #[default]
    SourceQuery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentBufferClampReason {
    Disabled,
    CacheBudget,
    CompleteReadLimit,
    RemoteTransferLimit,
    ContigBoundary,
    TooDense,
    SourceError,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentQueryPhase {
    #[default]
    Queued,
    FetchingReads,
    SelectingResidentReads,
    BuildingSummaries,
    PreparingReadLayout,
    ExtendingBrowsingBuffer,
    Ready,
}

impl AlignmentQueryPhase {
    pub const fn status_text(self) -> &'static str {
        match self {
            Self::Queued => "Preparing read request…",
            Self::FetchingReads => "Fetching reads…",
            Self::SelectingResidentReads => "Selecting resident reads…",
            Self::BuildingSummaries => "Building summaries…",
            Self::PreparingReadLayout => "Preparing read layout…",
            Self::ExtendingBrowsingBuffer => "Extending browsing buffer…",
            Self::Ready => "Ready",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentWorkingSetStats {
    pub cache_outcome: AlignmentCacheOutcome,
    pub resident_start: Option<u64>,
    pub resident_end: Option<u64>,
    pub resident_records: usize,
    pub resident_bytes: usize,
    pub requested_margin_left: u64,
    pub requested_margin_right: u64,
    pub effective_margin_left: u64,
    pub effective_margin_right: u64,
    pub clamp_reason: Option<AlignmentBufferClampReason>,
    pub resident_contract_revision: u64,
    pub target_contained: bool,
    pub lookup_micros: u64,
    pub projection_micros: u64,
    pub summary_micros: u64,
    pub summary_tile_hits: usize,
    pub summary_index_bytes: usize,
    pub extensions_requested: u64,
    pub extensions_completed: u64,
    pub extensions_cancelled: u64,
    pub extensions_too_dense: u64,
    pub extensions_failed: u64,
    pub source_queries: u64,
    /// Source queries required to make the most recent foreground target authoritative. This is
    /// zero for a contained resident-scene hit even if speculative margin replenishment follows.
    pub foreground_source_queries: u64,
    /// Cumulative source queries used only to acquire or extend speculative browsing margins.
    pub speculative_source_queries: u64,
    pub source_queries_avoided: u64,
    pub decoded_bytes_avoided: u64,
    pub speculative_records: usize,
    pub speculative_remote_bytes: u64,
    pub speculative_remote_requests: u64,
    pub resident_evictions: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanDirection {
    Backward,
    Forward,
}

enum AlignmentEngineRequest {
    ClearCache,
    SetCacheBudget {
        maximum_bytes: NonZeroUsize,
    },
    Open {
        path: PathBuf,
        options: AlignmentOpenOptions,
    },
    Query {
        request: PanelGeneration,
        clock: GenerationClock,
        interval: GenomicInterval,
        options: QueryOptions,
        browsing: AlignmentBrowsingPolicy,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum AlignmentEngineEvent {
    Opened {
        dictionary: ReferenceDictionary,
        index_overview: Option<Arc<AlignmentIndexOverview>>,
    },
    QueryCompleted {
        request: PanelGeneration,
        query: Arc<AlignmentQuery>,
        /// Fixed-neighbourhood records used to establish or extend the
        /// canonical row architecture independently of viewport scale.
        layout_query: Arc<AlignmentQuery>,
        cache_hit: bool,
        layout_cache_hit: bool,
        cache_stats: CacheStats,
        remote_io_stats: Option<RemoteIoStats>,
        working_set: AlignmentWorkingSetStats,
    },
    WorkingSetUpdated {
        request: PanelGeneration,
        /// The settled decoded resident after speculative edge acquisition.
        /// Consumers may prepare derived row/render state while the camera is
        /// idle instead of paying that cost on the next contained gesture.
        resident_query: Option<Arc<AlignmentQuery>>,
        cache_stats: CacheStats,
        remote_io_stats: Option<RemoteIoStats>,
        working_set: AlignmentWorkingSetStats,
    },
    QueryPhaseChanged {
        request: PanelGeneration,
        phase: AlignmentQueryPhase,
    },
    Error {
        request: Option<PanelGeneration>,
        message: String,
    },
    CacheCleared {
        cache_stats: CacheStats,
    },
    CacheBudgetChanged {
        cache_stats: CacheStats,
    },
}

/// A single-owner HTS actor. All blocking `HTSlib` work stays off the event/render thread.
pub struct AlignmentEngine {
    input: Option<Sender<AlignmentEngineRequest>>,
    output: CoalescingReceiver<AlignmentEngineEvent>,
    worker: Option<JoinHandle<()>>,
}

impl AlignmentEngine {
    pub fn spawn(queue_capacity: usize) -> Self {
        Self::spawn_with_cache_budget(
            queue_capacity,
            NonZeroUsize::new(ALIGNMENT_CACHE_BYTES).unwrap(),
        )
    }

    pub fn spawn_with_cache_budget(queue_capacity: usize, cache_budget: NonZeroUsize) -> Self {
        let capacity = queue_capacity.max(1);
        let (input_tx, input_rx) = bounded(capacity);
        let (output_tx, output_rx) = bounded_coalescing(capacity);
        let worker = thread::Builder::new()
            .name("rgv-hts-actor".to_owned())
            .spawn(move || worker_loop(&input_rx, &output_tx, cache_budget))
            .expect("failed to spawn RGV alignment actor");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
        }
    }

    pub fn try_open(
        &self,
        path: PathBuf,
        options: AlignmentOpenOptions,
    ) -> Result<(), EngineQueueError> {
        self.try_send(AlignmentEngineRequest::Open { path, options })
    }

    pub fn try_query(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        interval: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), EngineQueueError> {
        self.try_query_with_browsing(
            request,
            clock,
            interval,
            options,
            AlignmentBrowsingPolicy::default(),
        )
    }

    pub fn try_query_with_browsing(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        interval: GenomicInterval,
        options: QueryOptions,
        browsing: AlignmentBrowsingPolicy,
    ) -> Result<(), EngineQueueError> {
        self.try_send(AlignmentEngineRequest::Query {
            request,
            clock,
            interval,
            options,
            browsing,
        })
    }

    pub fn try_clear_cache(&self) -> Result<(), EngineQueueError> {
        self.try_send(AlignmentEngineRequest::ClearCache)
    }

    pub fn try_set_cache_budget(
        &self,
        maximum_bytes: NonZeroUsize,
    ) -> Result<(), EngineQueueError> {
        self.try_send(AlignmentEngineRequest::SetCacheBudget { maximum_bytes })
    }

    pub fn try_receive(&self) -> Result<Option<AlignmentEngineEvent>, EngineQueueError> {
        self.output
            .try_receive()
            .map_err(|_| EngineQueueError::Disconnected)
    }

    /// Closes this actor immediately and joins its worker from a small reaper
    /// thread. Desktop track removal uses this path so the event thread never
    /// waits for an in-flight local or remote HTS query to observe cancellation.
    #[must_use]
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.input.take();
        self.output.close();
        let worker = self.worker.take()?;
        match thread::Builder::new()
            .name("rgv-alignment-reaper".to_owned())
            .spawn(move || {
                if worker.join().is_err() {
                    tracing::warn!("alignment actor panicked while retiring");
                }
            }) {
            Ok(reaper) => Some(reaper),
            Err(error) => {
                // The actor input and receiver are already closed. Dropping the
                // worker handle detaches it, and it will still terminate after
                // the current cancellation boundary.
                tracing::warn!(%error, "could not spawn alignment reaper; actor detached");
                None
            }
        }
    }

    /// Waits for an engine event without allowing an integration runner to
    /// spin indefinitely. The desktop UI uses `try_receive` so it never blocks.
    pub fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<AlignmentEngineEvent>, EngineQueueError> {
        self.output
            .receive_timeout(timeout)
            .map_err(|_| EngineQueueError::Disconnected)
    }

    fn try_send(&self, request: AlignmentEngineRequest) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => EngineQueueError::Full,
            TrySendError::Disconnected(_) => EngineQueueError::Disconnected,
        })
    }
}

impl Drop for AlignmentEngine {
    fn drop(&mut self) {
        self.input.take();
        self.output.close();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("alignment actor panicked while shutting down");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResidentOrigin {
    NavigationAncestor,
    SpeculativeBuffer,
}

struct AlignmentResidentWindow {
    contract_fingerprint: u64,
    exact_query_fingerprint: Option<u64>,
    query: Arc<AlignmentQuery>,
    overlap_index: AlignmentOverlapIndex,
    summary_index: Option<Arc<AlignmentSummaryIndex>>,
    estimated_bytes: usize,
    last_used_revision: u64,
    origin: ResidentOrigin,
    cache_key: Option<QueryCacheKey>,
    derived_cache_keys: Vec<QueryCacheKey>,
}

#[derive(Clone)]
struct ResidentLayoutDonor {
    query: Arc<AlignmentQuery>,
    overlap_index: AlignmentOverlapIndex,
    summary_index: Option<Arc<AlignmentSummaryIndex>>,
}

impl ResidentLayoutDonor {
    fn from_window(window: &AlignmentResidentWindow) -> Self {
        Self {
            query: Arc::clone(&window.query),
            overlap_index: window.overlap_index.clone(),
            summary_index: window.summary_index.clone(),
        }
    }

    fn contains(&self, interval: GenomicInterval) -> bool {
        self.query.interval.contig() == interval.contig()
            && self.query.interval.start() <= interval.start()
            && self.query.interval.end() >= interval.end()
            && self.query.individual_reads_complete()
            && self.query.batch.filtered_record_spans_complete()
    }
}

impl AlignmentResidentWindow {
    fn new(
        contract_fingerprint: u64,
        exact_query_fingerprint: Option<u64>,
        query: Arc<AlignmentQuery>,
        origin: ResidentOrigin,
        cache_key: Option<QueryCacheKey>,
        overlapping_mate_policy: rgv_core::OverlappingMatePolicy,
        maximum_bytes: usize,
    ) -> Self {
        let overlap_index = AlignmentOverlapIndex::new(&query.batch);
        let base_estimated_bytes = query
            .estimated_bytes()
            .saturating_add(overlap_index.estimated_bytes());
        let summary_index = (base_estimated_bytes.saturating_add(MINIMUM_SUMMARY_INDEX_ALLOWANCE)
            <= maximum_bytes)
            .then(|| {
                AlignmentSummaryIndex::build(&query.batch, query.interval, overlapping_mate_policy)
                    .ok()
                    .map(Arc::new)
            })
            .flatten()
            .filter(|index| {
                base_estimated_bytes.saturating_add(index.estimated_bytes()) <= maximum_bytes
            });
        let estimated_bytes = base_estimated_bytes.saturating_add(
            summary_index
                .as_ref()
                .map_or(0, |index| index.estimated_bytes()),
        );
        Self {
            contract_fingerprint,
            exact_query_fingerprint,
            query,
            overlap_index,
            summary_index,
            estimated_bytes,
            last_used_revision: 0,
            origin,
            cache_key,
            derived_cache_keys: Vec::new(),
        }
    }

    fn contains(&self, interval: GenomicInterval, contract_fingerprint: u64) -> bool {
        self.contract_fingerprint == contract_fingerprint
            && self.query.interval.contig() == interval.contig()
            && self.query.interval.start() <= interval.start()
            && self.query.interval.end() >= interval.end()
            && self.query.individual_reads_complete()
            && self.query.batch.filtered_record_spans_complete()
    }
}

#[derive(Default)]
struct PanelNavigationState {
    previous_interval: Option<GenomicInterval>,
    previous_direction: Option<PanDirection>,
    last_foreground: Weak<AlignmentQuery>,
    last_foreground_key: Option<QueryCacheKey>,
    last_layout: Weak<AlignmentQuery>,
    last_layout_key: Option<QueryCacheKey>,
    extensions_requested: u64,
    extensions_completed: u64,
    extensions_cancelled: u64,
    extensions_too_dense: u64,
    extensions_failed: u64,
    last_foreground_source_queries: u64,
    speculative_source_queries: u64,
    failed_extension: Option<(u64, GenomicInterval, AlignmentBufferClampReason)>,
    source_queries_avoided: u64,
    decoded_bytes_avoided: u64,
    speculative_records: usize,
    speculative_remote_bytes: u64,
    speculative_remote_requests: u64,
    last_clamp_reason: Option<AlignmentBufferClampReason>,
}

struct AlignmentResidentStore {
    windows: HashMap<PanelId, AlignmentResidentWindow>,
    resident_bytes: usize,
    maximum_bytes: NonZeroUsize,
    revision: u64,
    hits: u64,
    evictions: u64,
}

impl AlignmentResidentStore {
    fn new(maximum_bytes: NonZeroUsize) -> Self {
        Self {
            windows: HashMap::new(),
            resident_bytes: 0,
            maximum_bytes,
            revision: 0,
            hits: 0,
            evictions: 0,
        }
    }

    fn clear(&mut self, cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>) {
        self.windows.clear();
        self.resident_bytes = 0;
        self.rebalance_cache(cache);
    }

    fn set_maximum_bytes(
        &mut self,
        maximum_bytes: NonZeroUsize,
        cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    ) {
        self.maximum_bytes = maximum_bytes;
        while self.resident_bytes > maximum_bytes.get() {
            let Some(panel_id) = self
                .windows
                .iter()
                .min_by_key(|(_, window)| {
                    (
                        match window.origin {
                            ResidentOrigin::SpeculativeBuffer => 0_u8,
                            ResidentOrigin::NavigationAncestor => 1_u8,
                        },
                        window.last_used_revision,
                    )
                })
                .map(|(panel_id, _)| *panel_id)
            else {
                break;
            };
            self.remove(panel_id, cache, true);
        }
        self.rebalance_cache(cache);
    }

    fn remove(
        &mut self,
        panel_id: PanelId,
        cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
        eviction: bool,
    ) {
        if let Some(window) = self.windows.remove(&panel_id) {
            self.resident_bytes = self.resident_bytes.saturating_sub(window.estimated_bytes);
            for key in window.derived_cache_keys {
                let _ = cache.remove(&key);
            }
            self.evictions = self.evictions.saturating_add(u64::from(eviction));
            self.rebalance_cache(cache);
        }
    }

    fn admit(
        &mut self,
        panel_id: PanelId,
        mut window: AlignmentResidentWindow,
        cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    ) -> bool {
        if window.estimated_bytes > self.maximum_bytes.get() {
            return false;
        }
        let replaced_bytes = self
            .windows
            .get(&panel_id)
            .map_or(0, |resident| resident.estimated_bytes);
        while self
            .resident_bytes
            .saturating_sub(replaced_bytes)
            .saturating_add(window.estimated_bytes)
            > self.maximum_bytes.get()
        {
            let Some(eviction_panel) = self
                .windows
                .iter()
                .filter(|(panel, _)| **panel != panel_id)
                .min_by_key(|(_, window)| {
                    (
                        match window.origin {
                            ResidentOrigin::SpeculativeBuffer => 0_u8,
                            ResidentOrigin::NavigationAncestor => 1_u8,
                        },
                        window.last_used_revision,
                    )
                })
                .map(|(panel, _)| *panel)
            else {
                break;
            };
            self.remove(eviction_panel, cache, true);
        }
        if self
            .resident_bytes
            .saturating_sub(replaced_bytes)
            .saturating_add(window.estimated_bytes)
            > self.maximum_bytes.get()
        {
            return false;
        }
        let preserves_existing_storage = self.windows.get(&panel_id).is_some_and(|existing| {
            window
                .query
                .batch
                .retains_all_storage_from(&existing.query.batch)
        });
        if preserves_existing_storage && let Some(existing) = self.windows.get_mut(&panel_id) {
            window
                .derived_cache_keys
                .append(&mut existing.derived_cache_keys);
        }
        self.remove(panel_id, cache, false);
        if let Some(key) = window.cache_key.take() {
            let _ = cache.remove(&key);
        }
        self.revision = self.revision.saturating_add(1);
        window.last_used_revision = self.revision;
        self.resident_bytes = self.resident_bytes.saturating_add(window.estimated_bytes);
        self.windows.insert(panel_id, window);
        self.rebalance_cache(cache);
        true
    }

    fn get_containing(
        &mut self,
        panel_id: PanelId,
        interval: GenomicInterval,
        contract_fingerprint: u64,
    ) -> Option<&AlignmentResidentWindow> {
        let window = self.windows.get_mut(&panel_id)?;
        if !window.contains(interval, contract_fingerprint) {
            return None;
        }
        self.revision = self.revision.saturating_add(1);
        window.last_used_revision = self.revision;
        self.hits = self.hits.saturating_add(1);
        Some(window)
    }

    fn panel(&self, panel_id: PanelId) -> Option<&AlignmentResidentWindow> {
        self.windows.get(&panel_id)
    }

    fn register_derived_cache_key(&mut self, panel_id: PanelId, key: QueryCacheKey) {
        let Some(window) = self.windows.get_mut(&panel_id) else {
            return;
        };
        if !window.derived_cache_keys.contains(&key) {
            window.derived_cache_keys.push(key);
        }
    }

    fn rebalance_cache(&self, cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>) {
        let available = self
            .maximum_bytes
            .get()
            .saturating_sub(self.resident_bytes)
            .max(1);
        cache.set_maximum_weight(NonZeroUsize::new(available).unwrap());
    }

    fn cache_stats(
        &self,
        cache: &WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    ) -> CacheStats {
        let exact = cache.stats();
        CacheStats {
            hits: exact.hits.saturating_add(self.hits),
            misses: exact.misses,
            evictions: exact.evictions.saturating_add(self.evictions),
            entries: exact.entries.saturating_add(self.windows.len()),
            resident_weight: exact.resident_weight.saturating_add(self.resident_bytes),
            maximum_weight: self.maximum_bytes.get(),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn worker_loop(
    input: &Receiver<AlignmentEngineRequest>,
    output: &CoalescingSender<AlignmentEngineEvent>,
    cache_budget: NonZeroUsize,
) {
    let mut source: Option<ProductionAlignmentSource> = None;
    let mut reference: Option<FastaReference> = None;
    let mut cache = WeightedLruCache::new(cache_budget);
    let mut residents = AlignmentResidentStore::new(cache_budget);
    let mut navigation_by_panel: HashMap<PanelId, PanelNavigationState> = HashMap::new();
    let mut source_queries = 0_u64;
    while let Ok(request) = input.recv() {
        let event = match request {
            AlignmentEngineRequest::ClearCache => {
                residents.clear(&mut cache);
                cache.clear();
                navigation_by_panel.clear();
                AlignmentEngineEvent::CacheCleared {
                    cache_stats: residents.cache_stats(&cache),
                }
            }
            AlignmentEngineRequest::SetCacheBudget { maximum_bytes } => {
                residents.set_maximum_bytes(maximum_bytes, &mut cache);
                AlignmentEngineEvent::CacheBudgetChanged {
                    cache_stats: residents.cache_stats(&cache),
                }
            }
            AlignmentEngineRequest::Open { path, options } => {
                let reference_options = FastaOpenOptions {
                    remote_index_url: options.remote_reference_index_url.clone(),
                    remote_range: options.remote_range.clone(),
                };
                let opened_reference = options
                    .reference_path
                    .as_ref()
                    .map(|path| FastaReference::open_with_options(path, &reference_options))
                    .transpose();
                match (
                    ProductionAlignmentSource::open(path, &options),
                    opened_reference,
                ) {
                    (Ok(opened), Ok(new_reference)) => {
                        let dictionary = opened.reference_dictionary().clone();
                        let index_overview = opened.index_overview().cloned().map(Arc::new);
                        if let Err(message) = validate_reference_dictionary(
                            &dictionary,
                            new_reference
                                .as_ref()
                                .map(FastaReference::reference_dictionary),
                        ) {
                            AlignmentEngineEvent::Error {
                                request: None,
                                message,
                            }
                        } else {
                            source = Some(opened);
                            reference = new_reference;
                            residents.clear(&mut cache);
                            cache.clear();
                            navigation_by_panel.clear();
                            source_queries = 0;
                            AlignmentEngineEvent::Opened {
                                dictionary,
                                index_overview,
                            }
                        }
                    }
                    (Err(error), _) | (_, Err(error)) => AlignmentEngineEvent::Error {
                        request: None,
                        message: error.to_string(),
                    },
                }
            }
            AlignmentEngineRequest::Query {
                request,
                clock,
                interval,
                options,
                browsing,
            } => {
                let Some(source) = source.as_mut() else {
                    send_event(
                        output,
                        AlignmentEngineEvent::Error {
                            request: Some(request),
                            message: "alignment source is not open".to_owned(),
                        },
                    );
                    continue;
                };
                let navigation = navigation_by_panel.entry(request.panel_id()).or_default();
                let previous_interval = navigation.previous_interval;
                let direction = navigation_direction(previous_interval, interval);
                let stable_direction =
                    direction.filter(|value| Some(*value) == navigation.previous_direction);
                navigation.previous_interval = Some(interval);
                navigation.previous_direction = direction;
                let key = QueryCacheKey {
                    interval,
                    options_fingerprint: options.fingerprint(),
                };
                let foreground_source_queries_before = source_queries;
                let mut publish_phase = |phase| {
                    send_event(
                        output,
                        AlignmentEngineEvent::QueryPhaseChanged { request, phase },
                    );
                };
                match resolve_foreground_query(
                    &mut cache,
                    &mut residents,
                    request.panel_id(),
                    key,
                    source,
                    reference.as_ref(),
                    &options,
                    &clock,
                    request.generation(),
                    navigation,
                    &mut source_queries,
                    &mut publish_phase,
                ) {
                    Ok((query, cache_outcome, derivation_metrics, resident_layout_donor))
                        if clock.is_current(request.generation()) && !query.stats.cancelled =>
                    {
                        let preferred_layout_interval = canonical_layout_horizon_interval(
                            interval,
                            source.reference_dictionary(),
                            source.is_remote(),
                        );
                        let layout_result = if !query.individual_reads_complete() {
                            Ok((
                                Arc::clone(&query),
                                cache_outcome != AlignmentCacheOutcome::SourceQuery,
                            ))
                        } else {
                            publish_phase(AlignmentQueryPhase::PreparingReadLayout);
                            let layout_options = layout_horizon_query_options(options.clone());
                            let retained_layout = navigation
                                .last_layout_key
                                .zip(navigation.last_layout.upgrade());
                            match resident_layout_donor
                                .as_ref()
                                .filter(|resident| resident.contains(preferred_layout_interval))
                            {
                                Some(resident) => cached_complete_layout_horizon_from_resident(
                                    &mut cache,
                                    &mut residents,
                                    request.panel_id(),
                                    preferred_layout_interval,
                                    canonical_layout_horizon_interval(
                                        interval,
                                        source.reference_dictionary(),
                                        true,
                                    ),
                                    layout_options,
                                    retained_layout,
                                    resident,
                                    &clock,
                                    request.generation(),
                                ),
                                None => cached_complete_layout_horizon(
                                    &mut cache,
                                    source,
                                    reference.as_ref(),
                                    interval,
                                    preferred_layout_interval,
                                    layout_options,
                                    retained_layout,
                                    &clock,
                                    request.generation(),
                                    &mut source_queries,
                                ),
                            }
                        };
                        match layout_result {
                            Ok((layout_query, layout_cache_hit))
                                if clock.is_current(request.generation())
                                    && !layout_query.stats.cancelled =>
                            {
                                navigation.last_foreground_source_queries =
                                    source_queries.saturating_sub(foreground_source_queries_before);
                                navigation.last_foreground = Arc::downgrade(&query);
                                navigation.last_foreground_key = Some(key);
                                navigation.last_layout = Arc::downgrade(&layout_query);
                                navigation.last_layout_key = Some(QueryCacheKey {
                                    interval: layout_query.interval,
                                    options_fingerprint: layout_horizon_query_options(
                                        options.clone(),
                                    )
                                    .fingerprint(),
                                });
                                let working_set = working_set_stats(
                                    &residents,
                                    request.panel_id(),
                                    interval,
                                    browsing,
                                    cache_outcome,
                                    derivation_metrics,
                                    navigation,
                                    source_queries,
                                );
                                let event = AlignmentEngineEvent::QueryCompleted {
                                    request,
                                    query: Arc::clone(&query),
                                    layout_query: Arc::clone(&layout_query),
                                    cache_hit: cache_outcome != AlignmentCacheOutcome::SourceQuery,
                                    layout_cache_hit,
                                    cache_stats: residents.cache_stats(&cache),
                                    remote_io_stats: combined_remote_io_stats(
                                        source.remote_io_stats(),
                                        reference
                                            .as_ref()
                                            .and_then(FastaReference::remote_io_stats),
                                    ),
                                    working_set,
                                };
                                send_event(output, event);
                                if clock.is_current(request.generation()) {
                                    if browsing.buffer_percent > 0 {
                                        publish_phase(AlignmentQueryPhase::ExtendingBrowsingBuffer);
                                    }
                                    acquire_resident_buffer(
                                        &mut cache,
                                        &mut residents,
                                        request.panel_id(),
                                        source,
                                        &query,
                                        &options,
                                        browsing,
                                        stable_direction,
                                        &clock,
                                        request.generation(),
                                        navigation,
                                        &mut source_queries,
                                    );
                                }
                                if clock.is_current(request.generation()) {
                                    send_event(
                                        output,
                                        AlignmentEngineEvent::WorkingSetUpdated {
                                            request,
                                            resident_query: residents
                                                .panel(request.panel_id())
                                                .map(|resident| Arc::clone(&resident.query)),
                                            cache_stats: residents.cache_stats(&cache),
                                            remote_io_stats: combined_remote_io_stats(
                                                source.remote_io_stats(),
                                                reference
                                                    .as_ref()
                                                    .and_then(FastaReference::remote_io_stats),
                                            ),
                                            working_set: working_set_stats(
                                                &residents,
                                                request.panel_id(),
                                                interval,
                                                browsing,
                                                cache_outcome,
                                                derivation_metrics,
                                                navigation,
                                                source_queries,
                                            ),
                                        },
                                    );
                                    publish_phase(AlignmentQueryPhase::Ready);
                                }
                                continue;
                            }
                            Err(error) if clock.is_current(request.generation()) => {
                                AlignmentEngineEvent::Error {
                                    request: Some(request),
                                    message: error.to_string(),
                                }
                            }
                            Ok(_) | Err(_) => continue,
                        }
                    }
                    Err(error) if clock.is_current(request.generation()) => {
                        AlignmentEngineEvent::Error {
                            request: Some(request),
                            message: error.to_string(),
                        }
                    }
                    Ok(_) | Err(_) => continue,
                }
            }
        };
        send_event(output, event);
    }
}

fn combined_remote_io_stats(
    left: Option<RemoteIoStats>,
    right: Option<RemoteIoStats>,
) -> Option<RemoteIoStats> {
    match (left, right) {
        (None, None) => None,
        (Some(stats), None) | (None, Some(stats)) => Some(stats),
        (Some(left), Some(right)) => Some(RemoteIoStats {
            requests: left.requests.saturating_add(right.requests),
            response_bytes: left.response_bytes.saturating_add(right.response_bytes),
            cache_hits: left.cache_hits.saturating_add(right.cache_hits),
            cache_misses: left.cache_misses.saturating_add(right.cache_misses),
            evictions: left.evictions.saturating_add(right.evictions),
            retries: left.retries.saturating_add(right.retries),
            cancellations: left.cancellations.saturating_add(right.cancellations),
        }),
    }
}

fn validate_reference_dictionary(
    alignment: &ReferenceDictionary,
    reference: Option<&ReferenceDictionary>,
) -> Result<(), String> {
    let Some(reference) = reference else {
        return Ok(());
    };
    let mut shared_contigs = 0_usize;
    for alignment_contig in alignment.iter() {
        let Some(reference_id) = reference.id_by_name(alignment_contig.name()) else {
            continue;
        };
        let Some(reference_contig) = reference.get(reference_id) else {
            return Err("reference FASTA dictionary is internally inconsistent".to_owned());
        };
        shared_contigs += 1;
        if alignment_contig.length() != reference_contig.length() {
            return Err(format!(
                "reference length mismatch for {}: alignment header {}, FASTA {}",
                alignment_contig.name(),
                alignment_contig.length(),
                reference_contig.length()
            ));
        }
    }
    (shared_contigs > 0)
        .then_some(())
        .ok_or_else(|| "reference FASTA has no contigs in common with the alignment header".into())
}

fn query_with_reference(
    source: &mut ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    interval: GenomicInterval,
    options: QueryOptions,
    clock: &GenerationClock,
    generation: ViewportGeneration,
) -> Result<AlignmentQuery, AlignmentError> {
    let include_reference = options.include_reference;
    let query_clock = clock.clone();
    let cancellation: RemoteCancellation = Arc::new(move || query_clock.is_current(generation));
    let mut query = source.query_with_cancellation(interval, options, &cancellation)?;
    if include_reference
        && let Some(reference) = reference
        && clock.is_current(generation)
    {
        reference.set_remote_cancellation(Some(cancellation));
        let contig = source
            .reference_dictionary()
            .get(interval.contig())
            .ok_or(AlignmentError::UnknownContig(interval.contig().get()))?;
        query.reference = reference.fetch_slice_by_name(contig.name(), interval)?;
    }
    Ok(query)
}

#[allow(clippy::too_many_arguments)]
fn resolve_foreground_query(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    residents: &mut AlignmentResidentStore,
    panel_id: PanelId,
    key: QueryCacheKey,
    source: &mut ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    options: &QueryOptions,
    clock: &GenerationClock,
    generation: ViewportGeneration,
    navigation: &mut PanelNavigationState,
    source_queries: &mut u64,
    publish_phase: &mut dyn FnMut(AlignmentQueryPhase),
) -> Result<
    (
        Arc<AlignmentQuery>,
        AlignmentCacheOutcome,
        AlignmentDerivationMetrics,
        Option<ResidentLayoutDonor>,
    ),
    AlignmentError,
> {
    if let Some(query) = cache.get(&key).cloned() {
        let resident_layout_donor = residents
            .get_containing(panel_id, key.interval, options.fingerprint())
            .map(ResidentLayoutDonor::from_window);
        navigation.source_queries_avoided = navigation.source_queries_avoided.saturating_add(1);
        navigation.decoded_bytes_avoided = navigation
            .decoded_bytes_avoided
            .saturating_add(u64::try_from(query.estimated_bytes()).unwrap_or(u64::MAX));
        return Ok((
            query,
            AlignmentCacheOutcome::Exact,
            AlignmentDerivationMetrics::default(),
            resident_layout_donor,
        ));
    }

    let contract_fingerprint = options.fingerprint();
    let resident_is_compatible = residents
        .panel(panel_id)
        .is_some_and(|window| window.contains(key.interval, contract_fingerprint));
    let previous_ancestor = navigation
        .last_foreground
        .upgrade()
        .zip(navigation.last_foreground_key)
        .filter(|(previous, previous_key)| {
            previous_key.options_fingerprint == contract_fingerprint
                && previous.interval.contig() == key.interval.contig()
                && previous.interval.start() <= key.interval.start()
                && previous.interval.end() >= key.interval.end()
                && previous.individual_reads_complete()
                && previous.batch.filtered_record_spans_complete()
        });
    // A compatible resident window remains the owner of decoded storage. Exact
    // camera views are registered below as lightweight shared cache entries, so
    // replacing a wider speculative window with the previous foreground would
    // throw away useful margins merely to protect a view that is already pinned.
    if let Some((previous, previous_key)) = previous_ancestor.filter(|_| !resident_is_compatible) {
        let _ = residents.admit(
            panel_id,
            AlignmentResidentWindow::new(
                contract_fingerprint,
                Some(previous_key.options_fingerprint),
                previous,
                ResidentOrigin::NavigationAncestor,
                Some(previous_key),
                options.overlapping_mate_policy,
                residents.maximum_bytes.get(),
            ),
            cache,
        );
    } else if !resident_is_compatible {
        residents.remove(panel_id, cache, true);
    }

    let resident = residents
        .get_containing(panel_id, key.interval, contract_fingerprint)
        .map(|window| {
            (
                Arc::clone(&window.query),
                window.overlap_index.clone(),
                window.summary_index.clone(),
                window.exact_query_fingerprint == Some(key.options_fingerprint)
                    && window.query.interval == key.interval,
                window.estimated_bytes,
                window.origin,
            )
        });
    if let Some((
        resident_query,
        overlap_index,
        summary_index,
        exact_return,
        resident_bytes,
        origin,
    )) = resident
    {
        navigation.source_queries_avoided = navigation.source_queries_avoided.saturating_add(1);
        navigation.decoded_bytes_avoided = navigation
            .decoded_bytes_avoided
            .saturating_add(u64::try_from(resident_bytes).unwrap_or(u64::MAX));
        if exact_return {
            let donor = ResidentLayoutDonor {
                query: Arc::clone(&resident_query),
                overlap_index,
                summary_index,
            };
            return Ok((
                Arc::clone(&resident_query),
                AlignmentCacheOutcome::ResidentExactReturn,
                AlignmentDerivationMetrics::default(),
                Some(donor),
            ));
        }
        let exact_reference = fetch_reference_for_target(
            source,
            reference,
            key.interval,
            options.include_reference,
            clock,
            generation,
        )?;
        let (query, metrics) = derive_alignment_query_with_summary_index_and_phase(
            &resident_query,
            &overlap_index,
            summary_index.as_deref(),
            key.interval,
            options,
            exact_reference,
            &|| clock.is_current(generation),
            &mut |phase| {
                publish_phase(match phase {
                    AlignmentDerivationPhase::SelectingResidentReads => {
                        AlignmentQueryPhase::SelectingResidentReads
                    }
                    AlignmentDerivationPhase::BuildingSummaries => {
                        AlignmentQueryPhase::BuildingSummaries
                    }
                });
            },
        )?;
        let query = Arc::new(query);
        if !query.stats.cancelled {
            if cache
                .insert(key, Arc::clone(&query), query.shared_view_estimated_bytes())
                .is_ok()
            {
                residents.register_derived_cache_key(panel_id, key);
            }
        }
        return Ok((
            query,
            match origin {
                ResidentOrigin::NavigationAncestor => AlignmentCacheOutcome::ResidentContainment,
                ResidentOrigin::SpeculativeBuffer => AlignmentCacheOutcome::ResidentExtension,
            },
            metrics,
            Some(ResidentLayoutDonor {
                query: resident_query,
                overlap_index,
                summary_index,
            }),
        ));
    }

    publish_phase(AlignmentQueryPhase::FetchingReads);
    *source_queries = source_queries.saturating_add(1);
    let query = Arc::new(query_with_reference(
        source,
        reference,
        key.interval,
        options.clone(),
        clock,
        generation,
    )?);
    if !query.stats.cancelled {
        let _ = cache.insert(key, Arc::clone(&query), query.estimated_bytes());
    }
    Ok((
        query,
        AlignmentCacheOutcome::SourceQuery,
        AlignmentDerivationMetrics::default(),
        None,
    ))
}

fn fetch_reference_for_target(
    source: &ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    interval: GenomicInterval,
    include_reference: bool,
    clock: &GenerationClock,
    generation: ViewportGeneration,
) -> Result<Option<rgv_hts::ReferenceSlice>, AlignmentError> {
    let Some(reference) = reference.filter(|_| include_reference) else {
        return Ok(None);
    };
    if !clock.is_current(generation) {
        return Ok(None);
    }
    let query_clock = clock.clone();
    reference.set_remote_cancellation(Some(Arc::new(move || query_clock.is_current(generation))));
    let contig = source
        .reference_dictionary()
        .get(interval.contig())
        .ok_or(AlignmentError::UnknownContig(interval.contig().get()))?;
    reference.fetch_slice_by_name(contig.name(), interval)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn acquire_resident_buffer(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    residents: &mut AlignmentResidentStore,
    panel_id: PanelId,
    source: &mut ProductionAlignmentSource,
    foreground: &Arc<AlignmentQuery>,
    options: &QueryOptions,
    browsing: AlignmentBrowsingPolicy,
    stable_direction: Option<PanDirection>,
    clock: &GenerationClock,
    generation: ViewportGeneration,
    navigation: &mut PanelNavigationState,
    source_queries: &mut u64,
) {
    let buffer_percent = browsing
        .buffer_percent
        .min(rgv_core::MAXIMUM_READ_BROWSING_BUFFER_PERCENT);
    if buffer_percent == 0 {
        if residents
            .panel(panel_id)
            .is_some_and(|window| window.origin == ResidentOrigin::SpeculativeBuffer)
        {
            residents.remove(panel_id, cache, true);
        }
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::Disabled);
        return;
    }
    if !foreground.individual_reads_complete() || foreground.stats.cancelled {
        return;
    }

    let interval = foreground.interval;
    // A complete exact ancestor is more valuable than speculative margins: it
    // guarantees an instant zoom-back and already provides a useful containing
    // window. Do not replace it with a non-exact buffer after deriving a child.
    if residents.panel(panel_id).is_some_and(|window| {
        window.origin == ResidentOrigin::NavigationAncestor
            && window.contains(interval, options.fingerprint())
    }) {
        return;
    }
    let requested_margin = interval.len().saturating_mul(u64::from(buffer_percent)) / 100;
    let (mut desired_left, mut desired_right) = (requested_margin, requested_margin);
    if source.is_remote() {
        match stable_direction.unwrap_or(PanDirection::Forward) {
            PanDirection::Backward => desired_right = 0,
            PanDirection::Forward => desired_left = 0,
        }
    } else if let Some(direction) = stable_direction {
        let total = desired_left.saturating_add(desired_right);
        let trailing = total / 4;
        match direction {
            PanDirection::Backward => {
                desired_left = total.saturating_sub(trailing);
                desired_right = trailing;
            }
            PanDirection::Forward => {
                desired_left = trailing;
                desired_right = total.saturating_sub(trailing);
            }
        }
    }

    if let Some(window) = residents.panel(panel_id) {
        let effective_left = interval
            .start()
            .saturating_sub(window.query.interval.start());
        let effective_right = window.query.interval.end().saturating_sub(interval.end());
        if window.contains(interval, options.fingerprint())
            && effective_left >= desired_left
            && effective_right >= desired_right
        {
            navigation.last_clamp_reason = None;
            return;
        }
    }

    let decoded_budget = if source.is_remote() {
        residents
            .maximum_bytes
            .get()
            .min(REMOTE_SPECULATIVE_DECODED_BYTES)
    } else {
        residents.maximum_bytes.get()
    };
    // A resident projection can retain a much broader Arc-backed arena. That
    // retained storage is charged once by cache ownership, but using it as the
    // viewport density estimate would falsely clamp a 2 GiB budget based on
    // records outside this viewport.
    let foreground_bytes = foreground.logical_decoded_estimated_bytes().max(1);
    if foreground_bytes > decoded_budget {
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::CacheBudget);
        return;
    }
    let viewport_span = interval.len();
    let byte_limited_span = viewport_span
        .saturating_mul(u64::try_from(decoded_budget).unwrap_or(u64::MAX))
        / u64::try_from(foreground_bytes).unwrap_or(u64::MAX).max(1);
    let record_limited_span = if foreground.stats.records_eligible == 0 {
        u64::MAX
    } else {
        viewport_span.saturating_mul(u64::try_from(options.max_records.get()).unwrap_or(u64::MAX))
            / u64::try_from(foreground.stats.records_eligible)
                .unwrap_or(u64::MAX)
                .max(1)
    };
    let requested_span = viewport_span
        .saturating_add(desired_left)
        .saturating_add(desired_right);
    let allowed_span = requested_span
        .min(byte_limited_span.max(viewport_span))
        .min(record_limited_span.max(viewport_span));
    if allowed_span <= viewport_span {
        navigation.last_clamp_reason = Some(if byte_limited_span <= record_limited_span {
            AlignmentBufferClampReason::CacheBudget
        } else {
            AlignmentBufferClampReason::CompleteReadLimit
        });
        return;
    }
    if allowed_span < requested_span {
        let allowed_extra = allowed_span.saturating_sub(viewport_span);
        let requested_extra = desired_left.saturating_add(desired_right).max(1);
        desired_left = desired_left.saturating_mul(allowed_extra) / requested_extra;
        desired_right = allowed_extra.saturating_sub(desired_left);
        navigation.last_clamp_reason = Some(if byte_limited_span <= record_limited_span {
            AlignmentBufferClampReason::CacheBudget
        } else {
            AlignmentBufferClampReason::CompleteReadLimit
        });
    } else {
        navigation.last_clamp_reason = None;
    }

    let Some(contig) = source.reference_dictionary().get(interval.contig()) else {
        return;
    };
    let start = interval.start().saturating_sub(desired_left);
    let end = interval
        .end()
        .saturating_add(desired_right)
        .min(contig.length());
    if start == interval.start() && end == interval.end() {
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::ContigBoundary);
        return;
    }
    if interval.start() < desired_left || end < interval.end().saturating_add(desired_right) {
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::ContigBoundary);
    }
    let Ok(resident_interval) = GenomicInterval::new(interval.contig(), start, end) else {
        return;
    };
    if residents.panel(panel_id).is_some_and(|window| {
        window.contains(interval, options.fingerprint())
            && window.query.interval.start() <= resident_interval.start()
            && window.query.interval.end() >= resident_interval.end()
    }) {
        return;
    }
    if let Some((fingerprint, failed, reason)) = navigation.failed_extension
        && fingerprint == options.fingerprint()
        && failed == resident_interval
    {
        navigation.last_clamp_reason = Some(reason);
        return;
    }

    let mut resident_options = options.clone();
    if options.collect_modifications {
        resident_options.detail = QueryDetail::Full;
    } else if options.collect_alleles && resident_options.detail == QueryDetail::Layout {
        resident_options.detail = QueryDetail::Sequence;
    }
    resident_options.coverage_bins = NonZeroUsize::new(1).unwrap();
    resident_options.collect_alleles = false;
    resident_options.collect_modifications = false;
    resident_options.include_reference = false;

    if source.is_remote() {
        source.set_remote_transfer_budget(Some(RemoteTransferBudget {
            maximum_requests: REMOTE_SPECULATIVE_REQUESTS,
            maximum_response_bytes: REMOTE_SPECULATIVE_RESPONSE_BYTES,
        }));
    }
    let remote_before = source.remote_io_stats();
    let merge_base = residents
        .panel(panel_id)
        .filter(|window| window.contains(interval, options.fingerprint()))
        .map(|window| Arc::clone(&window.query))
        .unwrap_or_else(|| Arc::clone(foreground));
    let can_merge_slabs = merge_base.batch.filtered_record_spans_complete();
    let result = if can_merge_slabs {
        acquire_missing_slabs(
            source,
            merge_base,
            resident_interval,
            &resident_options,
            clock,
            generation,
            navigation,
            source_queries,
        )
    } else {
        navigation.extensions_requested = navigation.extensions_requested.saturating_add(1);
        navigation.speculative_source_queries =
            navigation.speculative_source_queries.saturating_add(1);
        *source_queries = source_queries.saturating_add(1);
        query_with_reference(
            source,
            None,
            resident_interval,
            resident_options.clone(),
            clock,
            generation,
        )
        .map(Arc::new)
    };
    source.set_remote_transfer_budget(None);
    let remote_after = source.remote_io_stats();
    let remote_delta = remote_stats_delta(remote_before, remote_after);
    navigation.speculative_remote_bytes = navigation
        .speculative_remote_bytes
        .saturating_add(remote_delta.response_bytes);
    navigation.speculative_remote_requests = navigation
        .speculative_remote_requests
        .saturating_add(remote_delta.requests);
    let query = match result {
        Ok(query) => query,
        Err(error) if remote_transfer_budget_exceeded(&error) => {
            navigation.last_clamp_reason = Some(AlignmentBufferClampReason::RemoteTransferLimit);
            navigation.failed_extension = Some((
                options.fingerprint(),
                resident_interval,
                AlignmentBufferClampReason::RemoteTransferLimit,
            ));
            return;
        }
        Err(_) => {
            navigation.extensions_failed = navigation.extensions_failed.saturating_add(1);
            navigation.last_clamp_reason = Some(AlignmentBufferClampReason::SourceError);
            navigation.failed_extension = Some((
                options.fingerprint(),
                resident_interval,
                AlignmentBufferClampReason::SourceError,
            ));
            return;
        }
    };
    if query.stats.cancelled || !clock.is_current(generation) {
        navigation.extensions_cancelled = navigation.extensions_cancelled.saturating_add(1);
        return;
    }
    if !query.individual_reads_complete() {
        navigation.extensions_too_dense = navigation.extensions_too_dense.saturating_add(1);
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::TooDense);
        navigation.failed_extension = Some((
            options.fingerprint(),
            resident_interval,
            AlignmentBufferClampReason::TooDense,
        ));
        return;
    }
    if source.is_remote()
        && (remote_delta.response_bytes > REMOTE_SPECULATIVE_RESPONSE_BYTES
            || remote_delta.requests > REMOTE_SPECULATIVE_REQUESTS)
    {
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::RemoteTransferLimit);
        navigation.failed_extension = Some((
            options.fingerprint(),
            resident_interval,
            AlignmentBufferClampReason::RemoteTransferLimit,
        ));
        return;
    }
    navigation.speculative_records = query.batch.len();
    navigation.failed_extension = None;
    let retains_foreground_storage = query.batch.retains_all_storage_from(&foreground.batch);
    let admitted = residents.admit(
        panel_id,
        AlignmentResidentWindow::new(
            options.fingerprint(),
            None,
            Arc::clone(&query),
            ResidentOrigin::SpeculativeBuffer,
            None,
            options.overlapping_mate_policy,
            residents.maximum_bytes.get(),
        ),
        cache,
    );
    if admitted {
        navigation.extensions_completed = navigation.extensions_completed.saturating_add(1);
        if retains_foreground_storage && let Some(key) = navigation.last_foreground_key {
            let _ = cache.remove(&key);
            if cache
                .insert(
                    key,
                    Arc::clone(foreground),
                    foreground.shared_view_estimated_bytes(),
                )
                .is_ok()
            {
                residents.register_derived_cache_key(panel_id, key);
            }
        }
    } else {
        navigation.last_clamp_reason = Some(AlignmentBufferClampReason::CacheBudget);
    }
}

#[allow(clippy::too_many_arguments)]
fn acquire_missing_slabs(
    source: &mut ProductionAlignmentSource,
    mut base: Arc<AlignmentQuery>,
    target: GenomicInterval,
    options: &QueryOptions,
    clock: &GenerationClock,
    generation: ViewportGeneration,
    navigation: &mut PanelNavigationState,
    source_queries: &mut u64,
) -> Result<Arc<AlignmentQuery>, AlignmentError> {
    let mut slabs = Vec::with_capacity(2);
    if target.start() < base.interval.start() {
        slabs.push(
            GenomicInterval::new(target.contig(), target.start(), base.interval.start())
                .map_err(|_| AlignmentError::CoordinateOverflow)?,
        );
    }
    if target.end() > base.interval.end() {
        slabs.push(
            GenomicInterval::new(target.contig(), base.interval.end(), target.end())
                .map_err(|_| AlignmentError::CoordinateOverflow)?,
        );
    }
    for slab_interval in slabs {
        navigation.extensions_requested = navigation.extensions_requested.saturating_add(1);
        navigation.speculative_source_queries =
            navigation.speculative_source_queries.saturating_add(1);
        *source_queries = source_queries.saturating_add(1);
        let slab = query_with_reference(
            source,
            None,
            slab_interval,
            options.clone(),
            clock,
            generation,
        )
        .map_err(|_| AlignmentError::CoordinateOverflow)?;
        if slab.stats.cancelled || !clock.is_current(generation) {
            return Ok(Arc::new(slab));
        }
        if !slab.individual_reads_complete() {
            return Ok(Arc::new(slab));
        }
        let interval = GenomicInterval::new(
            target.contig(),
            base.interval.start().min(slab.interval.start()),
            base.interval.end().max(slab.interval.end()),
        )
        .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let batch = base
            .batch
            .merge_complete(&slab.batch)
            .map_err(AlignmentError::BatchOverflow)?;
        if batch.len() > options.max_records.get() {
            return Ok(Arc::new(AlignmentQuery {
                interval,
                header: Arc::clone(&base.header),
                batch: rgv_hts::AlignmentBatch::default(),
                pileup: rgv_hts::PileupSummary::default(),
                junctions: rgv_hts::JunctionSummary::default(),
                pairs: rgv_hts::PairSummary::default(),
                reference: None,
                stats: rgv_hts::QueryStats {
                    records_eligible: batch.len(),
                    complete_read_limit: options.max_records.get(),
                    individual_reads: rgv_hts::IndividualReadStatus::TooDense {
                        limit: options.max_records.get(),
                    },
                    truncated: true,
                    ..rgv_hts::QueryStats::default()
                },
            }));
        }
        let normalization_mapped_records = base
            .junctions
            .provenance
            .normalization_mapped_records
            .or(slab.junctions.provenance.normalization_mapped_records);
        let mut junctions = rgv_hts::JunctionSummary::default();
        junctions.provenance.normalization_mapped_records = normalization_mapped_records;
        let records = batch.len();
        base = Arc::new(AlignmentQuery {
            interval,
            header: Arc::clone(&base.header),
            batch,
            // These summaries belong to no committed target. Exact target
            // derivation always rebuilds them before this resident data can be
            // published.
            pileup: rgv_hts::PileupSummary::default(),
            junctions,
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: rgv_hts::QueryStats {
                records_seen: records,
                records_eligible: records,
                records_emitted: records,
                complete_read_limit: options.max_records.get(),
                individual_reads: rgv_hts::IndividualReadStatus::Complete,
                ..rgv_hts::QueryStats::default()
            },
        });
    }
    Ok(base)
}

fn working_set_stats(
    residents: &AlignmentResidentStore,
    panel_id: PanelId,
    target: GenomicInterval,
    browsing: AlignmentBrowsingPolicy,
    cache_outcome: AlignmentCacheOutcome,
    derivation_metrics: AlignmentDerivationMetrics,
    navigation: &PanelNavigationState,
    source_queries: u64,
) -> AlignmentWorkingSetStats {
    let requested = target
        .len()
        .saturating_mul(u64::from(browsing.buffer_percent))
        / 100;
    let resident = residents.panel(panel_id);
    AlignmentWorkingSetStats {
        cache_outcome,
        resident_start: resident.map(|window| window.query.interval.start()),
        resident_end: resident.map(|window| window.query.interval.end()),
        resident_records: resident.map_or(0, |window| window.query.batch.len()),
        resident_bytes: resident.map_or(0, |window| window.estimated_bytes),
        requested_margin_left: requested,
        requested_margin_right: requested,
        effective_margin_left: resident.map_or(0, |window| {
            target.start().saturating_sub(window.query.interval.start())
        }),
        effective_margin_right: resident.map_or(0, |window| {
            window.query.interval.end().saturating_sub(target.end())
        }),
        clamp_reason: navigation.last_clamp_reason,
        resident_contract_revision: resident.map_or(0, |window| window.last_used_revision),
        target_contained: resident.is_some_and(|window| {
            window.query.interval.contig() == target.contig()
                && window.query.interval.start() <= target.start()
                && window.query.interval.end() >= target.end()
        }),
        lookup_micros: derivation_metrics.lookup_micros,
        projection_micros: derivation_metrics.projection_micros,
        summary_micros: derivation_metrics.summary_micros,
        summary_tile_hits: derivation_metrics.summary_tile_hits,
        summary_index_bytes: resident
            .and_then(|window| window.summary_index.as_ref())
            .map_or(0, |index| index.estimated_bytes()),
        extensions_requested: navigation.extensions_requested,
        extensions_completed: navigation.extensions_completed,
        extensions_cancelled: navigation.extensions_cancelled,
        extensions_too_dense: navigation.extensions_too_dense,
        extensions_failed: navigation.extensions_failed,
        source_queries,
        foreground_source_queries: navigation.last_foreground_source_queries,
        speculative_source_queries: navigation.speculative_source_queries,
        source_queries_avoided: navigation.source_queries_avoided,
        decoded_bytes_avoided: navigation.decoded_bytes_avoided,
        speculative_records: navigation.speculative_records,
        speculative_remote_bytes: navigation.speculative_remote_bytes,
        speculative_remote_requests: navigation.speculative_remote_requests,
        resident_evictions: residents.evictions,
    }
}

fn remote_stats_delta(
    before: Option<RemoteIoStats>,
    after: Option<RemoteIoStats>,
) -> RemoteIoStats {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    RemoteIoStats {
        requests: after.requests.saturating_sub(before.requests),
        response_bytes: after.response_bytes.saturating_sub(before.response_bytes),
        cache_hits: after.cache_hits.saturating_sub(before.cache_hits),
        cache_misses: after.cache_misses.saturating_sub(before.cache_misses),
        evictions: after.evictions.saturating_sub(before.evictions),
        retries: after.retries.saturating_sub(before.retries),
        cancellations: after.cancellations.saturating_sub(before.cancellations),
    }
}

fn remote_transfer_budget_exceeded(error: &AlignmentError) -> bool {
    matches!(
        error,
        AlignmentError::Remote(RemoteError::TransferBudgetExceeded)
    ) || error
        .to_string()
        .contains("speculative transfer reached its request or response-byte budget")
}

#[allow(clippy::too_many_arguments)]
fn cached_or_query_alignment(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    key: QueryCacheKey,
    source: &mut ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    options: QueryOptions,
    clock: &GenerationClock,
    generation: ViewportGeneration,
    source_queries: &mut u64,
) -> Result<(Arc<AlignmentQuery>, bool), AlignmentError> {
    if let Some(query) = cache.get(&key).cloned() {
        return Ok((query, true));
    }
    *source_queries = source_queries.saturating_add(1);
    let query = Arc::new(query_with_reference(
        source,
        reference,
        key.interval,
        options,
        clock,
        generation,
    )?);
    // Cancellation is generation-local, not a reusable source result. Caching
    // it would make the next identical viewport hit an empty cancelled entry
    // and never receive a settled Complete or Too-dense state.
    if !query.stats.cancelled {
        let _ = cache.insert(key, Arc::clone(&query), query.estimated_bytes());
    }
    Ok((query, false))
}

#[allow(clippy::too_many_arguments)]
fn cached_complete_layout_horizon_from_resident(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    residents: &mut AlignmentResidentStore,
    panel_id: PanelId,
    preferred: GenomicInterval,
    minimum: GenomicInterval,
    options: QueryOptions,
    retained: Option<(QueryCacheKey, Arc<AlignmentQuery>)>,
    donor: &ResidentLayoutDonor,
    clock: &GenerationClock,
    generation: ViewportGeneration,
) -> Result<(Arc<AlignmentQuery>, bool), AlignmentError> {
    let mut last = None;
    for interval in [preferred, minimum] {
        if last
            .as_ref()
            .is_some_and(|(query, _): &(Arc<AlignmentQuery>, bool)| query.interval == interval)
        {
            continue;
        }
        if !donor.contains(interval) {
            continue;
        }
        let key = QueryCacheKey {
            interval,
            options_fingerprint: options.fingerprint(),
        };
        if let Some((retained_key, retained_query)) = &retained
            && *retained_key == key
            && retained_query.individual_reads_complete()
            && !retained_query.stats.cancelled
        {
            return Ok((Arc::clone(retained_query), true));
        }
        if let Some(query) = cache.get(&key).cloned() {
            return Ok((query, true));
        }
        let (query, _) = derive_alignment_query_with_summary_index_and_phase(
            &donor.query,
            &donor.overlap_index,
            donor.summary_index.as_deref(),
            interval,
            &options,
            None,
            &|| clock.is_current(generation),
            &mut |_| {},
        )?;
        let query = Arc::new(query);
        if !query.stats.cancelled
            && cache
                .insert(key, Arc::clone(&query), query.shared_view_estimated_bytes())
                .is_ok()
        {
            residents.register_derived_cache_key(panel_id, key);
        }
        let result = (query, false);
        if result.0.individual_reads_complete() || !clock.is_current(generation) {
            return Ok(result);
        }
        last = Some(result);
    }
    last.ok_or(AlignmentError::IncompatibleResidentQuery)
}

#[allow(clippy::too_many_arguments)]
fn cached_complete_layout_horizon(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    source: &mut ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    requested: GenomicInterval,
    preferred: GenomicInterval,
    options: QueryOptions,
    retained: Option<(QueryCacheKey, Arc<AlignmentQuery>)>,
    clock: &GenerationClock,
    generation: ViewportGeneration,
    source_queries: &mut u64,
) -> Result<(Arc<AlignmentQuery>, bool), AlignmentError> {
    let minimum = canonical_layout_horizon_interval(requested, source.reference_dictionary(), true);
    let mut last = None;
    for interval in [preferred, minimum] {
        if last
            .as_ref()
            .is_some_and(|(query, _): &(Arc<AlignmentQuery>, bool)| query.interval == interval)
        {
            continue;
        }
        let key = QueryCacheKey {
            interval,
            options_fingerprint: options.fingerprint(),
        };
        if let Some((retained_key, retained_query)) = &retained
            && *retained_key == key
            && retained_query.individual_reads_complete()
            && !retained_query.stats.cancelled
        {
            return Ok((Arc::clone(retained_query), true));
        }
        let result = cached_or_query_alignment(
            cache,
            key,
            source,
            reference,
            options.clone(),
            clock,
            generation,
            source_queries,
        )?;
        if result.0.individual_reads_complete() || !clock.is_current(generation) {
            return Ok(result);
        }
        last = Some(result);
    }
    Ok(last.expect("the complete-horizon candidate list is non-empty"))
}

fn layout_horizon_query_options(mut options: QueryOptions) -> QueryOptions {
    options.detail = if options.layout_requires_sequence {
        QueryDetail::Sequence
    } else {
        QueryDetail::Layout
    };
    options.coverage_bins = NonZeroUsize::new(1).expect("one coverage bin is non-zero");
    options.collect_alleles = false;
    options.collect_modifications = false;
    options.include_reference = false;
    options
}

fn canonical_layout_horizon_interval(
    requested: GenomicInterval,
    dictionary: &ReferenceDictionary,
    remote: bool,
) -> GenomicInterval {
    let Some(contig) = dictionary.get(requested.contig()) else {
        return requested;
    };
    let first_tile = requested.start() / ALIGNMENT_LAYOUT_TILE_BASES;
    let last_tile = requested.end().saturating_sub(1) / ALIGNMENT_LAYOUT_TILE_BASES;
    // Local sources pay for one navigation tile on both sides up front. Remote
    // sources stay cache-aligned but extend only when approached so background
    // layout cannot create an unbounded range-I/O surprise.
    let margin = u64::from(!remote);
    let start = first_tile
        .saturating_sub(margin)
        .saturating_mul(ALIGNMENT_LAYOUT_TILE_BASES);
    let end = last_tile
        .saturating_add(1)
        .saturating_add(margin)
        .saturating_mul(ALIGNMENT_LAYOUT_TILE_BASES)
        .min(contig.length());
    GenomicInterval::new(requested.contig(), start, end).unwrap_or(requested)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct QueryCacheKey {
    interval: GenomicInterval,
    options_fingerprint: u64,
}

fn navigation_direction(
    previous: Option<GenomicInterval>,
    current: GenomicInterval,
) -> Option<PanDirection> {
    let previous = previous.filter(|interval| interval.contig() == current.contig())?;
    match current.start().cmp(&previous.start()) {
        std::cmp::Ordering::Less => Some(PanDirection::Backward),
        std::cmp::Ordering::Greater => Some(PanDirection::Forward),
        std::cmp::Ordering::Equal => None,
    }
}

fn send_event(output: &CoalescingSender<AlignmentEngineEvent>, event: AlignmentEngineEvent) {
    let result = if let AlignmentEngineEvent::QueryCompleted { request, .. } = &event {
        output.send_latest_for(request.panel_id().get(), event)
    } else {
        output.send_control(event)
    };
    if result.is_err() {
        tracing::debug!("alignment event receiver closed");
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EngineQueueError {
    #[error("the bounded alignment queue is full")]
    Full,
    #[error("the alignment actor is disconnected")]
    Disconnected,
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, PanelId};
    use rgv_hts::{
        AlignmentBatch, AlignmentHeaderMetadata, JunctionSummary, PairSummary, PileupSummary,
        QueryStats,
    };

    use super::*;

    #[test]
    fn canonical_layout_horizon_is_fixed_across_the_motivating_zoom_pair() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 248_956_422_u64)]);
        let narrow = GenomicInterval::new(ContigId::new(0), 36_339_308, 36_343_089).unwrap();
        let broad = GenomicInterval::new(ContigId::new(0), 36_337_133, 36_345_430).unwrap();
        let narrow_horizon = canonical_layout_horizon_interval(narrow, &dictionary, false);
        let broad_horizon = canonical_layout_horizon_interval(broad, &dictionary, false);

        assert_eq!(narrow_horizon, broad_horizon);
        assert!(narrow_horizon.start() <= broad.start());
        assert!(narrow_horizon.end() >= broad.end());
        assert_eq!(
            (narrow_horizon.start(), narrow_horizon.end()),
            (36_274_176, 36_405_248)
        );
    }

    #[test]
    fn remote_layout_horizon_uses_only_intersecting_fixed_tiles() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 248_956_422_u64)]);
        let requested = GenomicInterval::new(ContigId::new(0), 36_339_308, 36_343_089).unwrap();
        let horizon = canonical_layout_horizon_interval(requested, &dictionary, true);
        assert_eq!((horizon.start(), horizon.end()), (36_306_944, 36_372_480));
    }

    #[test]
    fn direction_requires_a_matching_contig_and_movement() {
        let first = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
        let later = GenomicInterval::new(ContigId::new(0), 120, 220).unwrap();
        let other = GenomicInterval::new(ContigId::new(1), 120, 220).unwrap();
        assert_eq!(
            navigation_direction(Some(first), later),
            Some(PanDirection::Forward)
        );
        assert_eq!(navigation_direction(Some(first), first), None);
        assert_eq!(navigation_direction(Some(first), other), None);
    }

    #[test]
    fn rejected_resident_replacement_keeps_the_existing_window() {
        let budget = NonZeroUsize::new(100).unwrap();
        let mut residents = AlignmentResidentStore::new(budget);
        let mut cache = WeightedLruCache::new(budget);
        let panel = PanelId::new(1);
        let query = |interval| {
            Arc::new(AlignmentQuery {
                interval,
                header: Arc::new(AlignmentHeaderMetadata::default()),
                batch: AlignmentBatch::default(),
                pileup: PileupSummary::default(),
                junctions: JunctionSummary::default(),
                pairs: PairSummary::default(),
                reference: None,
                stats: QueryStats::default(),
            })
        };
        let first_interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
        let mut first = AlignmentResidentWindow::new(
            1,
            None,
            query(first_interval),
            ResidentOrigin::SpeculativeBuffer,
            None,
            rgv_core::OverlappingMatePolicy::default(),
            budget.get(),
        );
        first.estimated_bytes = 60;
        assert!(residents.admit(panel, first, &mut cache));

        let replacement_interval = GenomicInterval::new(ContigId::new(0), 50, 250).unwrap();
        let mut replacement = AlignmentResidentWindow::new(
            1,
            None,
            query(replacement_interval),
            ResidentOrigin::SpeculativeBuffer,
            None,
            rgv_core::OverlappingMatePolicy::default(),
            budget.get(),
        );
        replacement.estimated_bytes = 101;
        assert!(!residents.admit(panel, replacement, &mut cache));

        let retained = residents.panel(panel).expect("old resident remains usable");
        assert_eq!(retained.query.interval, first_interval);
        assert_eq!(residents.resident_bytes, 60);
        assert_eq!(residents.evictions, 0);
    }

    #[test]
    fn live_budget_resize_evicts_speculation_before_navigation_ancestors() {
        let initial = NonZeroUsize::new(100).unwrap();
        let mut residents = AlignmentResidentStore::new(initial);
        let mut cache = WeightedLruCache::new(initial);
        let query = |start| {
            Arc::new(AlignmentQuery {
                interval: GenomicInterval::new(ContigId::new(0), start, start + 10).unwrap(),
                header: Arc::new(AlignmentHeaderMetadata::default()),
                batch: AlignmentBatch::default(),
                pileup: PileupSummary::default(),
                junctions: JunctionSummary::default(),
                pairs: PairSummary::default(),
                reference: None,
                stats: QueryStats::default(),
            })
        };
        let mut navigation = AlignmentResidentWindow::new(
            1,
            None,
            query(10),
            ResidentOrigin::NavigationAncestor,
            None,
            rgv_core::OverlappingMatePolicy::default(),
            initial.get(),
        );
        navigation.estimated_bytes = 30;
        assert!(residents.admit(PanelId::new(1), navigation, &mut cache));
        let mut speculation = AlignmentResidentWindow::new(
            1,
            None,
            query(30),
            ResidentOrigin::SpeculativeBuffer,
            None,
            rgv_core::OverlappingMatePolicy::default(),
            initial.get(),
        );
        speculation.estimated_bytes = 60;
        assert!(residents.admit(PanelId::new(2), speculation, &mut cache));

        residents.set_maximum_bytes(NonZeroUsize::new(50).unwrap(), &mut cache);

        assert!(residents.panel(PanelId::new(1)).is_some());
        assert!(residents.panel(PanelId::new(2)).is_none());
        let stats = residents.cache_stats(&cache);
        assert_eq!(stats.maximum_weight, 50);
        assert_eq!(stats.resident_weight, 30);
        assert_eq!(residents.evictions, 1);

        residents.set_maximum_bytes(NonZeroUsize::new(200).unwrap(), &mut cache);
        assert_eq!(residents.cache_stats(&cache).maximum_weight, 200);
    }

    #[test]
    fn reference_dictionary_accepts_partial_and_reordered_contigs() {
        let alignment =
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 100), ("chr2".to_owned(), 200)]);
        let matching = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 100),
            ("chr2".to_owned(), 200),
            ("chrM".to_owned(), 20),
        ]);
        let partial = ReferenceDictionary::from_contigs([("chr2".to_owned(), 200)]);
        let reordered =
            ReferenceDictionary::from_contigs([("chr2".to_owned(), 200), ("chr1".to_owned(), 100)]);
        let wrong =
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 100), ("chr2".to_owned(), 201)]);
        let disjoint = ReferenceDictionary::from_contigs([("chrM".to_owned(), 20)]);
        assert!(validate_reference_dictionary(&alignment, Some(&matching)).is_ok());
        assert!(validate_reference_dictionary(&alignment, Some(&partial)).is_ok());
        assert!(validate_reference_dictionary(&alignment, Some(&reordered)).is_ok());
        assert!(validate_reference_dictionary(&alignment, Some(&wrong)).is_err());
        assert!(validate_reference_dictionary(&alignment, Some(&disjoint)).is_err());
    }

    #[test]
    fn query_burst_retains_newest_result_behind_control_event() {
        let (output, receiver) = bounded_coalescing(1);
        send_event(
            &output,
            AlignmentEngineEvent::CacheCleared {
                cache_stats: CacheStats::default(),
            },
        );
        for generation in 1..=32 {
            let query = Arc::new(AlignmentQuery {
                interval: GenomicInterval::new(ContigId::new(0), 0, 1).unwrap(),
                header: std::sync::Arc::default(),
                batch: AlignmentBatch::default(),
                pileup: PileupSummary::default(),
                junctions: rgv_hts::JunctionSummary::default(),
                pairs: rgv_hts::PairSummary::default(),
                reference: None,
                stats: QueryStats::default(),
            });
            send_event(
                &output,
                AlignmentEngineEvent::QueryCompleted {
                    request: PanelGeneration::new(
                        PanelId::new(1),
                        ViewportGeneration::new(generation),
                    ),
                    query: Arc::clone(&query),
                    layout_query: query,
                    cache_hit: false,
                    layout_cache_hit: false,
                    cache_stats: CacheStats::default(),
                    remote_io_stats: None,
                    working_set: AlignmentWorkingSetStats::default(),
                },
            );
        }

        assert!(matches!(
            receiver.try_receive().unwrap(),
            Some(AlignmentEngineEvent::CacheCleared { .. })
        ));
        assert!(matches!(
            receiver.try_receive().unwrap(),
            Some(AlignmentEngineEvent::QueryCompleted { request, .. })
                if request
                    == PanelGeneration::new(PanelId::new(1), ViewportGeneration::new(32))
        ));
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn full_query_commits_a_cached_layout_only_horizon_atomically() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(4);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let requested = GenomicInterval::new(contig, 2_150_000, 2_150_010).unwrap();
        let clock = GenerationClock::default();

        for expected_cache_hit in [false, true] {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query(
                    request,
                    clock.clone(),
                    requested,
                    QueryOptions {
                        detail: QueryDetail::Full,
                        ..QueryOptions::default()
                    },
                )
                .unwrap();
            let completed =
                loop {
                    match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                        Some(
                            event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. },
                        ) if seen == request => break event,
                        Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                        Some(_) | None => continue,
                    }
                };
            let AlignmentEngineEvent::QueryCompleted {
                query,
                layout_query,
                cache_hit,
                layout_cache_hit,
                ..
            } = completed
            else {
                unreachable!()
            };
            assert_eq!(cache_hit, expected_cache_hit);
            assert_eq!(layout_cache_hit, expected_cache_hit);
            assert_eq!(query.interval, requested);
            assert!(
                query.individual_reads_complete(),
                "foreground eligible records: {}",
                query.stats.records_eligible
            );
            assert!(layout_query.interval.start() <= query.interval.start());
            assert!(layout_query.interval.end() >= query.interval.end());
            if !layout_query.batch.is_empty() {
                assert!(matches!(
                    layout_query.materialize_record(0).unwrap().sequence,
                    rgv_hts::SequenceData::NotLoaded
                ));
            }
        }
    }

    #[test]
    fn query_events_report_source_and_resident_derivation_phases() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(8);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let broad = GenomicInterval::new(contig, 9_950, 10_050).unwrap();
        let narrow = GenomicInterval::new(contig, 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let clock = GenerationClock::default();
        let phases_for = |interval| {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    AlignmentBrowsingPolicy { buffer_percent: 0 },
                )
                .unwrap();
            let mut phases = Vec::new();
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(AlignmentEngineEvent::QueryPhaseChanged {
                        request: seen,
                        phase,
                    }) if seen == request => {
                        phases.push(phase);
                        if phase == AlignmentQueryPhase::Ready {
                            break phases;
                        }
                    }
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let source_phases = phases_for(broad);
        assert!(source_phases.contains(&AlignmentQueryPhase::FetchingReads));
        assert!(source_phases.contains(&AlignmentQueryPhase::PreparingReadLayout));

        let resident_phases = phases_for(narrow);
        assert!(resident_phases.contains(&AlignmentQueryPhase::SelectingResidentReads));
        assert!(resident_phases.contains(&AlignmentQueryPhase::BuildingSummaries));
        assert!(!resident_phases.contains(&AlignmentQueryPhase::FetchingReads));
    }

    #[test]
    fn complete_foreground_is_not_sampled_when_the_stable_horizon_is_too_dense() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(4);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let requested = GenomicInterval::new(contig, 2_150_000, 2_150_010).unwrap();
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.advance());
        engine
            .try_query(
                request,
                clock.clone(),
                requested,
                QueryOptions {
                    detail: QueryDetail::Full,
                    max_records: NonZeroUsize::new(100).unwrap(),
                    ..QueryOptions::default()
                },
            )
            .unwrap();
        let completed = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                    if seen == request =>
                {
                    break event;
                }
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let AlignmentEngineEvent::QueryCompleted {
            query,
            layout_query,
            ..
        } = completed
        else {
            unreachable!()
        };
        assert_eq!(query.interval, requested);
        assert!(
            query.individual_reads_complete(),
            "foreground eligible records: {}",
            query.stats.records_eligible
        );
        assert_eq!(query.batch.len(), query.stats.records_eligible);
        assert!(matches!(
            layout_query.stats.individual_reads,
            rgv_hts::IndividualReadStatus::TooDense { limit: 100 }
        ));
        assert!(layout_query.batch.is_empty());
        assert!(layout_query.stats.records_eligible > 100);
    }

    #[test]
    fn cancelled_foreground_is_not_reused_for_the_next_identical_generation() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(4);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let interval = GenomicInterval::new(contig, 2_150_000, 2_150_010).unwrap();
        let clock = GenerationClock::default();
        let stale_generation = clock.advance();
        let current_generation = clock.advance();
        let stale = PanelGeneration::new(PanelId::new(1), stale_generation);
        let current = PanelGeneration::new(PanelId::new(1), current_generation);
        let options = QueryOptions {
            detail: QueryDetail::Full,
            ..QueryOptions::default()
        };

        engine
            .try_query(stale, clock.clone(), interval, options.clone())
            .unwrap();
        engine
            .try_query(current, clock.clone(), interval, options)
            .unwrap();

        let completed = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(event @ AlignmentEngineEvent::QueryCompleted { request, .. })
                    if request == current =>
                {
                    break event;
                }
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let AlignmentEngineEvent::QueryCompleted {
            query, cache_hit, ..
        } = completed
        else {
            unreachable!()
        };
        assert!(!query.stats.cancelled);
        assert!(query.individual_reads_complete());
        assert!(
            !cache_hit,
            "a cancelled entry must never become a cache hit"
        );
    }

    #[test]
    fn too_dense_foreground_commits_no_individual_batch() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(4);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let requested = GenomicInterval::new(contig, 2_100_000, 2_301_100).unwrap();
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.advance());
        engine
            .try_query(
                request,
                clock.clone(),
                requested,
                QueryOptions {
                    detail: QueryDetail::Full,
                    max_records: NonZeroUsize::new(10).unwrap(),
                    ..QueryOptions::default()
                },
            )
            .unwrap();
        let completed = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                    if seen == request =>
                {
                    break event;
                }
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let AlignmentEngineEvent::QueryCompleted {
            query,
            layout_query,
            ..
        } = completed
        else {
            unreachable!()
        };
        assert_eq!(query.interval, requested);
        assert!(matches!(
            query.stats.individual_reads,
            rgv_hts::IndividualReadStatus::TooDense { limit: 10 }
        ));
        assert_eq!(query.stats.records_emitted, 0);
        assert!(query.batch.is_empty());
        assert!(query.pileup.provenance.all_eligible_records);
        assert!(Arc::ptr_eq(&query, &layout_query));
    }

    #[test]
    fn contained_navigation_and_exact_ancestor_return_avoid_source_queries() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(8);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let broad = GenomicInterval::new(contig, 9_950, 10_050).unwrap();
        let narrow = GenomicInterval::new(contig, 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let browsing = AlignmentBrowsingPolicy { buffer_percent: 0 };
        let clock = GenerationClock::default();
        let query = |interval| {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    browsing,
                )
                .unwrap();
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                        if seen == request =>
                    {
                        break event;
                    }
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let first = query(broad);
        let AlignmentEngineEvent::QueryCompleted {
            query: broad_query,
            working_set: first_stats,
            ..
        } = first
        else {
            unreachable!()
        };
        assert_eq!(
            first_stats.cache_outcome,
            AlignmentCacheOutcome::SourceQuery
        );

        let second = query(narrow);
        let AlignmentEngineEvent::QueryCompleted {
            query: narrow_query,
            working_set: second_stats,
            ..
        } = second
        else {
            unreachable!()
        };
        assert_eq!(
            second_stats.cache_outcome,
            AlignmentCacheOutcome::ResidentContainment
        );
        assert_eq!(second_stats.source_queries, first_stats.source_queries);
        assert_eq!(second_stats.foreground_source_queries, 0);
        assert!(narrow_query.batch.len() <= broad_query.batch.len());

        let third = query(broad);
        let AlignmentEngineEvent::QueryCompleted {
            query: returned,
            working_set: third_stats,
            ..
        } = third
        else {
            unreachable!()
        };
        assert_eq!(
            third_stats.cache_outcome,
            AlignmentCacheOutcome::ResidentExactReturn
        );
        assert_eq!(third_stats.source_queries, first_stats.source_queries);
        assert_eq!(third_stats.foreground_source_queries, 0);
        assert!(Arc::ptr_eq(&returned, &broad_query));

        let fourth = query(narrow);
        let AlignmentEngineEvent::QueryCompleted {
            query: cached_narrow,
            working_set: fourth_stats,
            ..
        } = fourth
        else {
            unreachable!()
        };
        assert_eq!(fourth_stats.cache_outcome, AlignmentCacheOutcome::Exact);
        assert_eq!(fourth_stats.summary_micros, 0);
        assert_eq!(fourth_stats.source_queries, first_stats.source_queries);
        assert_eq!(fourth_stats.foreground_source_queries, 0);
        assert!(Arc::ptr_eq(&cached_narrow, &narrow_query));
    }

    #[test]
    fn browsing_buffer_fetches_only_the_missing_edge_after_a_contained_pan() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(8);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let clock = GenerationClock::default();
        let query_and_update = |interval| {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    AlignmentBrowsingPolicy {
                        buffer_percent: 100,
                    },
                )
                .unwrap();
            let mut completed = None;
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                        if seen == request =>
                    {
                        completed = Some(event);
                    }
                    Some(AlignmentEngineEvent::WorkingSetUpdated {
                        request: seen,
                        working_set,
                        ..
                    }) if seen == request => {
                        break (
                            completed.expect("foreground precedes its buffer update"),
                            working_set,
                        );
                    }
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let first_interval = GenomicInterval::new(contig, 9_999, 10_001).unwrap();
        let (_, first) = query_and_update(first_interval);
        assert_eq!(first.extensions_requested, 2);
        assert_eq!(first.speculative_source_queries, 2);
        assert_eq!(first.extensions_completed, 1);
        assert!(first.resident_start < Some(first_interval.start()));
        assert!(first.resident_end > Some(first_interval.end()));

        let second_interval = GenomicInterval::new(contig, 10_000, 10_002).unwrap();
        let (completed, second) = query_and_update(second_interval);
        let AlignmentEngineEvent::QueryCompleted { working_set, .. } = completed else {
            unreachable!()
        };
        assert_eq!(
            working_set.cache_outcome,
            AlignmentCacheOutcome::ResidentExtension
        );
        assert_eq!(working_set.foreground_source_queries, 0);
        assert_eq!(second.extensions_requested, 3);
        assert_eq!(second.speculative_source_queries, 3);
        assert_eq!(second.extensions_completed, 2);
        assert_eq!(
            second.source_queries,
            first.source_queries.saturating_add(1)
        );
        assert!(second.resident_start <= first.resident_start);
        assert!(second.resident_end >= first.resident_end);

        let (returned, third) = query_and_update(first_interval);
        let AlignmentEngineEvent::QueryCompleted { working_set, .. } = returned else {
            unreachable!()
        };
        assert_eq!(working_set.cache_outcome, AlignmentCacheOutcome::Exact);
        assert_eq!(third.source_queries, second.source_queries);
    }

    #[test]
    fn speculative_residency_survives_contained_zoom_and_exact_view_survives_zoom_back() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(8);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let broad = GenomicInterval::new(contig, 9_950, 10_050).unwrap();
        let narrow = GenomicInterval::new(contig, 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let clock = GenerationClock::default();
        let query = |interval| {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    AlignmentBrowsingPolicy {
                        buffer_percent: 100,
                    },
                )
                .unwrap();
            let mut completed = None;
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                        if seen == request =>
                    {
                        completed = Some(event);
                    }
                    Some(AlignmentEngineEvent::WorkingSetUpdated {
                        request: seen,
                        working_set,
                        ..
                    }) if seen == request => {
                        break (
                            completed.expect("foreground precedes its buffer update"),
                            working_set,
                        );
                    }
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let (broad_event, _) = query(broad);
        let AlignmentEngineEvent::QueryCompleted {
            query: broad_query, ..
        } = broad_event
        else {
            unreachable!()
        };
        let (_, narrow_stats) = query(narrow);
        assert_eq!(
            narrow_stats.cache_outcome,
            AlignmentCacheOutcome::ResidentExtension
        );
        assert!(narrow_stats.resident_start <= Some(broad.start()));
        assert!(narrow_stats.resident_end >= Some(broad.end()));
        assert!(
            narrow_stats.resident_start < Some(broad.start())
                || narrow_stats.resident_end > Some(broad.end())
        );

        let (returned_event, returned_stats) = query(broad);
        let AlignmentEngineEvent::QueryCompleted {
            query: returned,
            working_set,
            ..
        } = returned_event
        else {
            unreachable!()
        };
        assert_eq!(working_set.cache_outcome, AlignmentCacheOutcome::Exact);
        assert_eq!(returned_stats.source_queries, narrow_stats.source_queries);
        assert!(Arc::ptr_eq(&returned, &broad_query));
    }

    #[test]
    fn resident_reads_derive_a_canonical_layout_horizon_instead_of_becoming_the_row_scene() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(8);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let broad = GenomicInterval::new(contig, 2_100_000, 2_220_000).unwrap();
        let fine = GenomicInterval::new(contig, 2_150_000, 2_155_000).unwrap();
        let expected_layout = canonical_layout_horizon_interval(fine, &dictionary, false);
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000_000).unwrap(),
            coverage_bins: NonZeroUsize::new(128).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let clock = GenerationClock::default();
        let query = |interval| {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    AlignmentBrowsingPolicy {
                        buffer_percent: 400,
                    },
                )
                .unwrap();
            let mut completed = None;
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                        if seen == request =>
                    {
                        completed = Some(event);
                    }
                    Some(AlignmentEngineEvent::WorkingSetUpdated {
                        request: seen,
                        working_set,
                        ..
                    }) if seen == request => {
                        break (
                            completed.expect("foreground precedes its buffer update"),
                            working_set,
                        );
                    }
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let (_, broad_working_set) = query(broad);
        let resident_start = broad_working_set
            .resident_start
            .expect("four-fold buffer admits a resident start");
        let resident_end = broad_working_set
            .resident_end
            .expect("four-fold buffer admits a resident end");
        assert!(resident_start <= expected_layout.start());
        assert!(resident_end >= expected_layout.end());
        assert!(resident_start < expected_layout.start() || resident_end > expected_layout.end());

        let (fine_event, _) = query(fine);
        let AlignmentEngineEvent::QueryCompleted {
            layout_query,
            working_set,
            ..
        } = fine_event
        else {
            unreachable!()
        };
        assert_eq!(working_set.foreground_source_queries, 0);
        assert_eq!(layout_query.interval, expected_layout);
        assert_ne!(
            (layout_query.interval.start(), layout_query.interval.end()),
            (resident_start, resident_end)
        );
        assert!(layout_query.individual_reads_complete());
    }

    #[test]
    fn zero_buffer_schedules_no_speculation_and_keeps_exact_cache_reuse() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let engine = AlignmentEngine::spawn(8);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let interval = GenomicInterval::new(contig, 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            ..QueryOptions::default()
        };
        let clock = GenerationClock::default();
        let query = || {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    AlignmentBrowsingPolicy { buffer_percent: 0 },
                )
                .unwrap();
            let mut completed = None;
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(event @ AlignmentEngineEvent::QueryCompleted { request: seen, .. })
                        if seen == request =>
                    {
                        completed = Some(event);
                    }
                    Some(AlignmentEngineEvent::WorkingSetUpdated {
                        request: seen,
                        working_set,
                        ..
                    }) if seen == request => {
                        break (
                            completed.expect("foreground precedes its working-set update"),
                            working_set,
                        );
                    }
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let (first_event, first) = query();
        let AlignmentEngineEvent::QueryCompleted {
            cache_stats: first_cache,
            ..
        } = first_event
        else {
            unreachable!()
        };
        assert_eq!(first.extensions_requested, 0);
        assert_eq!(first.extensions_completed, 0);
        assert_eq!(first.resident_records, 0);
        assert_eq!(
            first.clamp_reason,
            Some(AlignmentBufferClampReason::Disabled)
        );
        assert!(first_cache.resident_weight <= first_cache.maximum_weight);

        let (second_event, second) = query();
        let AlignmentEngineEvent::QueryCompleted {
            working_set: second_foreground,
            cache_stats: second_cache,
            ..
        } = second_event
        else {
            unreachable!()
        };
        assert_eq!(
            second_foreground.cache_outcome,
            AlignmentCacheOutcome::Exact
        );
        assert_eq!(second.source_queries, first.source_queries);
        assert_eq!(second.extensions_requested, 0);
        assert!(second_cache.resident_weight <= second_cache.maximum_weight);
    }

    #[test]
    fn four_x_buffer_and_repeated_navigation_reach_a_bounded_steady_state() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let cache_budget = NonZeroUsize::new(128 * 1024).unwrap();
        let engine = AlignmentEngine::spawn_with_cache_budget(8, cache_budget);
        engine
            .try_open(path, AlignmentOpenOptions::default())
            .unwrap();
        let dictionary = loop {
            match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                Some(AlignmentEngineEvent::Opened { dictionary, .. }) => break dictionary,
                Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                Some(_) | None => continue,
            }
        };
        let contig = dictionary.id_by_name("chr1").unwrap_or(ContigId::new(0));
        let intervals = [
            GenomicInterval::new(contig, 9_999, 10_001).unwrap(),
            GenomicInterval::new(contig, 10_000, 10_002).unwrap(),
        ];
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let clock = GenerationClock::default();
        let query = |interval| {
            let request = PanelGeneration::new(PanelId::new(1), clock.advance());
            engine
                .try_query_with_browsing(
                    request,
                    clock.clone(),
                    interval,
                    options.clone(),
                    AlignmentBrowsingPolicy {
                        buffer_percent: 400,
                    },
                )
                .unwrap();
            loop {
                match engine.receive_timeout(Duration::from_secs(10)).unwrap() {
                    Some(AlignmentEngineEvent::WorkingSetUpdated {
                        request: seen,
                        cache_stats,
                        working_set,
                        ..
                    }) if seen == request => break (cache_stats, working_set),
                    Some(AlignmentEngineEvent::Error { message, .. }) => panic!("{message}"),
                    Some(_) | None => continue,
                }
            }
        };

        let _ = query(intervals[0]);
        let (_, warm) = query(intervals[1]);
        let warm_bounds = (warm.resident_start, warm.resident_end, warm.resident_bytes);
        let warm_extensions = warm.extensions_completed;

        for cycle in 0..100 {
            let interval = intervals[cycle % intervals.len()];
            let (cache_stats, working_set) = query(interval);
            assert!(cache_stats.resident_weight <= cache_budget.get());
            assert_eq!(cache_stats.maximum_weight, cache_budget.get());
            assert!(working_set.effective_margin_left <= interval.len().saturating_mul(4));
            assert!(working_set.effective_margin_right <= interval.len().saturating_mul(4));
            assert_eq!(
                (
                    working_set.resident_start,
                    working_set.resident_end,
                    working_set.resident_bytes,
                ),
                warm_bounds
            );
            assert_ne!(
                working_set.cache_outcome,
                AlignmentCacheOutcome::SourceQuery
            );
            assert_eq!(working_set.extensions_completed, warm_extensions);
        }
    }

    #[test]
    fn retirement_closes_and_joins_the_actor_off_the_calling_thread() {
        let engine = AlignmentEngine::spawn(1);
        let reaper = engine.retire().expect("alignment reaper should spawn");
        assert!(reaper.join().is_ok());
    }
}
