use std::{
    collections::HashMap,
    num::NonZeroUsize,
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use rgv_core::{
    GenerationClock, GenomicInterval, PanelGeneration, ReferenceDictionary, ViewportGeneration,
};
use rgv_hts::{
    AlignmentError, AlignmentIndexOverview, AlignmentOpenOptions, AlignmentQuery, AlignmentSource,
    FastaOpenOptions, FastaReference, ProductionAlignmentSource, QueryDetail, QueryOptions,
    RemoteCancellation, RemoteIoStats,
};
use thiserror::Error;

use crate::{
    CacheStats, WeightedLruCache,
    mailbox::{CoalescingReceiver, CoalescingSender, bounded_coalescing},
};

const ALIGNMENT_CACHE_BYTES: usize = 320 * 1024 * 1024;
pub const ALIGNMENT_LAYOUT_TILE_BASES: u64 = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanDirection {
    Backward,
    Forward,
}

enum AlignmentEngineRequest {
    ClearCache,
    Open {
        path: PathBuf,
        options: AlignmentOpenOptions,
    },
    Query {
        request: PanelGeneration,
        clock: GenerationClock,
        interval: GenomicInterval,
        options: QueryOptions,
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
    },
    Error {
        request: Option<PanelGeneration>,
        message: String,
    },
    CacheCleared {
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
        self.try_send(AlignmentEngineRequest::Query {
            request,
            clock,
            interval,
            options,
        })
    }

    pub fn try_clear_cache(&self) -> Result<(), EngineQueueError> {
        self.try_send(AlignmentEngineRequest::ClearCache)
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

#[allow(clippy::too_many_lines)]
fn worker_loop(
    input: &Receiver<AlignmentEngineRequest>,
    output: &CoalescingSender<AlignmentEngineEvent>,
    cache_budget: NonZeroUsize,
) {
    let mut source: Option<ProductionAlignmentSource> = None;
    let mut reference: Option<FastaReference> = None;
    let mut cache = WeightedLruCache::new(cache_budget);
    let mut navigation_by_panel = HashMap::new();
    while let Ok(request) = input.recv() {
        let event = match request {
            AlignmentEngineRequest::ClearCache => {
                cache.clear();
                AlignmentEngineEvent::CacheCleared {
                    cache_stats: cache.stats(),
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
                            cache.clear();
                            navigation_by_panel.clear();
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
                let navigation = navigation_by_panel
                    .entry(request.panel_id())
                    .or_insert((None, None));
                let previous_interval = navigation.0;
                let direction = navigation_direction(previous_interval, interval);
                let stable_direction = direction.filter(|value| Some(*value) == navigation.1);
                *navigation = (Some(interval), direction);
                let key = QueryCacheKey {
                    interval,
                    options_fingerprint: options.fingerprint(),
                };
                match cached_or_query_alignment(
                    &mut cache,
                    key,
                    source,
                    reference.as_ref(),
                    options.clone(),
                    &clock,
                    request.generation(),
                ) {
                    Ok((query, cache_hit))
                        if clock.is_current(request.generation()) && !query.stats.cancelled =>
                    {
                        let preferred_layout_interval = canonical_layout_horizon_interval(
                            interval,
                            source.reference_dictionary(),
                            source.is_remote(),
                        );
                        let layout_result = if !query.individual_reads_complete() {
                            Ok((Arc::clone(&query), cache_hit))
                        } else {
                            let layout_options = layout_horizon_query_options(options.clone());
                            cached_complete_layout_horizon(
                                &mut cache,
                                source,
                                reference.as_ref(),
                                interval,
                                preferred_layout_interval,
                                layout_options,
                                &clock,
                                request.generation(),
                            )
                        };
                        match layout_result {
                            Ok((layout_query, layout_cache_hit))
                                if clock.is_current(request.generation())
                                    && !layout_query.stats.cancelled =>
                            {
                                let event = AlignmentEngineEvent::QueryCompleted {
                                    request,
                                    query,
                                    layout_query,
                                    cache_hit,
                                    layout_cache_hit,
                                    cache_stats: cache.stats(),
                                    remote_io_stats: combined_remote_io_stats(
                                        source.remote_io_stats(),
                                        reference
                                            .as_ref()
                                            .and_then(FastaReference::remote_io_stats),
                                    ),
                                };
                                send_event(output, event);
                                if let Some(prefetch_interval) = adjacent_prefetch_interval(
                                    interval,
                                    stable_direction,
                                    source.reference_dictionary(),
                                ) && clock.is_current(request.generation())
                                {
                                    let prefetch_key = QueryCacheKey {
                                        interval: prefetch_interval,
                                        options_fingerprint: options.fingerprint(),
                                    };
                                    // Background prefetch is best effort. Complete and Too-dense
                                    // results are both truth-bearing cache entries; cancellation or
                                    // source failure never changes the already committed foreground.
                                    let _ = cached_or_query_alignment(
                                        &mut cache,
                                        prefetch_key,
                                        source,
                                        reference.as_ref(),
                                        options,
                                        &clock,
                                        request.generation(),
                                    );
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
fn cached_or_query_alignment(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    key: QueryCacheKey,
    source: &mut ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    options: QueryOptions,
    clock: &GenerationClock,
    generation: ViewportGeneration,
) -> Result<(Arc<AlignmentQuery>, bool), AlignmentError> {
    if let Some(query) = cache.get(&key).cloned() {
        return Ok((query, true));
    }
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
fn cached_complete_layout_horizon(
    cache: &mut WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>,
    source: &mut ProductionAlignmentSource,
    reference: Option<&FastaReference>,
    requested: GenomicInterval,
    preferred: GenomicInterval,
    options: QueryOptions,
    clock: &GenerationClock,
    generation: ViewportGeneration,
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
        let result = cached_or_query_alignment(
            cache,
            key,
            source,
            reference,
            options.clone(),
            clock,
            generation,
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

fn adjacent_prefetch_interval(
    current: GenomicInterval,
    direction: Option<PanDirection>,
    dictionary: &ReferenceDictionary,
) -> Option<GenomicInterval> {
    let direction = direction?;
    let contig_length = dictionary.get(current.contig())?.length();
    let (start, end) = match direction {
        PanDirection::Backward => {
            let end = current.start();
            (end.saturating_sub(current.len()), end)
        }
        PanDirection::Forward => {
            let start = current.end();
            (
                start,
                start.saturating_add(current.len()).min(contig_length),
            )
        }
    };
    let adjacent = GenomicInterval::new(current.contig(), start, end).ok()?;
    (adjacent != current).then_some(adjacent)
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
    use rgv_hts::{AlignmentBatch, PileupSummary, QueryStats};

    use super::*;

    #[test]
    fn prefetch_uses_the_next_non_overlapping_equal_span_interval() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 10_000_u64)]);
        let current = GenomicInterval::new(ContigId::new(0), 2_100, 3_100).unwrap();
        let forward =
            adjacent_prefetch_interval(current, Some(PanDirection::Forward), &dictionary).unwrap();
        let backward =
            adjacent_prefetch_interval(current, Some(PanDirection::Backward), &dictionary).unwrap();
        assert_eq!((forward.start(), forward.end()), (3_100, 4_100));
        assert_eq!((backward.start(), backward.end()), (1_100, 2_100));
        assert!(adjacent_prefetch_interval(current, None, &dictionary).is_none());
    }

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
    fn retirement_closes_and_joins_the_actor_off_the_calling_thread() {
        let engine = AlignmentEngine::spawn(1);
        let reaper = engine.retire().expect("alignment reaper should spawn");
        assert!(reaper.join().is_ok());
    }
}
