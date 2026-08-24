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
    FastaOpenOptions, FastaReference, ProductionAlignmentSource, QueryOptions, RemoteCancellation,
    RemoteIoStats,
};
use thiserror::Error;

use crate::{
    CacheStats, WeightedLruCache,
    mailbox::{CoalescingReceiver, CoalescingSender, bounded_coalescing},
};

const ALIGNMENT_CACHE_BYTES: usize = 320 * 1024 * 1024;

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
        cache_hit: bool,
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
                let options_fingerprint = options.fingerprint();
                let navigation = navigation_by_panel
                    .entry(request.panel_id())
                    .or_insert((None, None));
                let direction = navigation_direction(navigation.0, interval);
                let stable_direction = direction.filter(|value| Some(*value) == navigation.1);
                *navigation = (Some(interval), direction);
                let expanded_interval = tile_aligned_prefetch_interval(
                    interval,
                    source.reference_dictionary(),
                    stable_direction,
                );
                let key = QueryCacheKey {
                    interval: expanded_interval,
                    options_fingerprint,
                };
                if let Some(query) = cache.get(&key).cloned() {
                    AlignmentEngineEvent::QueryCompleted {
                        request,
                        query,
                        cache_hit: true,
                        cache_stats: cache.stats(),
                        remote_io_stats: combined_remote_io_stats(
                            source.remote_io_stats(),
                            reference.as_ref().and_then(FastaReference::remote_io_stats),
                        ),
                    }
                } else {
                    let expanded_options =
                        expanded_query_options(options, interval, expanded_interval);
                    match query_with_reference(
                        source,
                        reference.as_ref(),
                        expanded_interval,
                        expanded_options,
                        &clock,
                        request.generation(),
                    ) {
                        Ok(query)
                            if clock.is_current(request.generation()) && !query.stats.cancelled =>
                        {
                            let query = Arc::new(query);
                            let _ = cache.insert(key, Arc::clone(&query), query.estimated_bytes());
                            AlignmentEngineEvent::QueryCompleted {
                                request,
                                query,
                                cache_hit: false,
                                cache_stats: cache.stats(),
                                remote_io_stats: combined_remote_io_stats(
                                    source.remote_io_stats(),
                                    reference.as_ref().and_then(FastaReference::remote_io_stats),
                                ),
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct QueryCacheKey {
    interval: GenomicInterval,
    options_fingerprint: u64,
}

fn tile_aligned_prefetch_interval(
    requested: GenomicInterval,
    dictionary: &ReferenceDictionary,
    direction: Option<PanDirection>,
) -> GenomicInterval {
    let Some(contig) = dictionary.get(requested.contig()) else {
        return requested;
    };
    let tile_width = requested
        .len()
        .checked_next_power_of_two()
        .unwrap_or(requested.len());
    let first_tile = requested.start() / tile_width;
    let last_tile = requested.end().saturating_sub(1) / tile_width;
    let first_prefetched_tile = if direction == Some(PanDirection::Backward) {
        first_tile.saturating_sub(1)
    } else {
        first_tile
    };
    let start = first_prefetched_tile.saturating_mul(tile_width);
    let end = last_tile
        .saturating_add(1 + u64::from(direction == Some(PanDirection::Forward)))
        .saturating_mul(tile_width)
        .min(contig.length());
    GenomicInterval::new(requested.contig(), start, end).unwrap_or(requested)
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

fn expanded_query_options(
    mut options: QueryOptions,
    requested: GenomicInterval,
    expanded: GenomicInterval,
) -> QueryOptions {
    let multiplier = usize::try_from(expanded.len().div_ceil(requested.len()))
        .unwrap_or(1)
        .max(1);
    let expanded_bins = options
        .coverage_bins
        .get()
        .saturating_mul(multiplier)
        .min(16_384)
        .min(usize::try_from(expanded.len()).unwrap_or(usize::MAX))
        .max(1);
    options.coverage_bins = NonZeroUsize::new(expanded_bins).unwrap();
    options
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
    fn prefetch_adds_only_the_stable_navigation_side() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 10_000_u64)]);
        let requested = GenomicInterval::new(ContigId::new(0), 2_100, 3_100).unwrap();
        let neutral = tile_aligned_prefetch_interval(requested, &dictionary, None);
        let forward =
            tile_aligned_prefetch_interval(requested, &dictionary, Some(PanDirection::Forward));
        let backward =
            tile_aligned_prefetch_interval(requested, &dictionary, Some(PanDirection::Backward));
        assert_eq!((neutral.start(), neutral.end()), (2_048, 4_096));
        assert_eq!((forward.start(), forward.end()), (2_048, 5_120));
        assert_eq!((backward.start(), backward.end()), (1_024, 4_096));
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
            send_event(
                &output,
                AlignmentEngineEvent::QueryCompleted {
                    request: PanelGeneration::new(
                        PanelId::new(1),
                        ViewportGeneration::new(generation),
                    ),
                    query: Arc::new(AlignmentQuery {
                        interval: GenomicInterval::new(ContigId::new(0), 0, 1).unwrap(),
                        header: std::sync::Arc::default(),
                        batch: AlignmentBatch::default(),
                        pileup: PileupSummary::default(),
                        junctions: rgv_hts::JunctionSummary::default(),
                        pairs: rgv_hts::PairSummary::default(),
                        reference: None,
                        stats: QueryStats::default(),
                    }),
                    cache_hit: false,
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
    fn retirement_closes_and_joins_the_actor_off_the_calling_thread() {
        let engine = AlignmentEngine::spawn(1);
        let reaper = engine.retire().expect("alignment reaper should spawn");
        assert!(reaper.join().is_ok());
    }
}
