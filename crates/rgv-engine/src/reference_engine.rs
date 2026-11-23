use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use rgv_alignment_scene::{ReferenceRevision, RevisionFingerprint};
use rgv_core::{ContigId, GenerationClock, GenomicInterval, PanelGeneration, ReferenceDictionary};
use rgv_hts::{
    FastaOpenOptions, FastaReference, ReferenceSlice, RemoteCancellation, RemoteIoStats,
};
use rgv_model::SpliceMotif;

use crate::{
    EngineQueueError,
    mailbox::{
        CoalescingReceiver, CoalescingSender, bounded_coalescing_to_ui, reap_worker,
        run_thread_root,
    },
};

/// Maximum number of bases fetched by one underlying FASTA operation.
///
/// A logical scene authority may be wider. The reference actor assembles it from bounded chunks
/// and checks cancellation between them, so consumers still receive one atomic slice without a
/// broad horizon becoming one uninterruptible local or remote source operation.
pub const MAX_REFERENCE_QUERY_BASES: u64 = 1_000_000;

/// Maximum number of bases one logical reference slice may hold.
///
/// A slice is one contiguous in-memory allocation published to every consumer of that panel, so
/// its size must not be whatever interval a caller happens to compute. The bound is far above
/// any horizon at which individual bases or reads are drawn; a wider request is refused with an
/// explicit error before anything is reserved or fetched.
pub const MAX_REFERENCE_SLICE_BASES: u64 = 128 * MAX_REFERENCE_QUERY_BASES;

/// Complete identity of one reference query. Panel generation alone is not sufficient because
/// one scene-camera generation may request several successive buffered horizons. Carrying the
/// expected open-instance revision and exact interval lets consumers reject an older completion
/// or failure without consulting mutable side state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceQueryIdentity {
    request: PanelGeneration,
    revision: ReferenceRevision,
    interval: GenomicInterval,
}

impl ReferenceQueryIdentity {
    #[must_use]
    pub const fn new(
        request: PanelGeneration,
        revision: ReferenceRevision,
        interval: GenomicInterval,
    ) -> Self {
        Self {
            request,
            revision,
            interval,
        }
    }

    #[must_use]
    pub const fn request(self) -> PanelGeneration {
        self.request
    }

    #[must_use]
    pub const fn revision(self) -> ReferenceRevision {
        self.revision
    }

    #[must_use]
    pub const fn interval(self) -> GenomicInterval {
        self.interval
    }
}

/// Process-local identity for the next successfully opened reference authority. Reference scene
/// caches are process-local, so an open-instance revision is sufficient to prevent two live or
/// successively opened FASTA handles from sharing semantic pages accidentally. This is
/// deliberately not presented as a content hash.
static NEXT_REFERENCE_OPEN_INSTANCE: AtomicU64 = AtomicU64::new(1);
const REFERENCE_OPEN_INSTANCE_DOMAIN: u64 = u64::from_le_bytes(*b"RGVREF01");

/// Most introns one splice-motif lookup may name. Each costs two two-base reads.
pub const MAX_SPLICE_MOTIF_QUERY_INTRONS: usize = 4_096;

/// Reference bases bounding a batch of introns, to give splice junctions a strand from their
/// canonical motif. Answered with [`ReferenceEngineEvent::SpliceMotifs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpliceMotifQuery {
    /// The open FASTA authority the caller expects; another revision's handle is not read.
    pub revision: ReferenceRevision,
    /// The alignment contig the introns belong to, echoed back for the caller's cache.
    pub contig: ContigId,
    /// The FASTA's own name for that contig.
    pub contig_name: String,
    /// Half-open `(start, end)` intron extents, zero-based.
    pub introns: Vec<(u64, u64)>,
}

enum ReferenceEngineRequest {
    Open {
        path: PathBuf,
        options: FastaOpenOptions,
    },
    SpliceMotifs(SpliceMotifQuery),
    Query {
        query: ReferenceQueryIdentity,
        clock: GenerationClock,
        contig_name: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceEngineEvent {
    Opened {
        revision: ReferenceRevision,
        dictionary: ReferenceDictionary,
        remote_io_stats: Option<RemoteIoStats>,
    },
    QueryCompleted {
        query: ReferenceQueryIdentity,
        slice: Arc<ReferenceSlice>,
        remote_io_stats: Option<RemoteIoStats>,
    },
    Error {
        /// `None` identifies an open failure; every query failure carries its complete identity.
        query: Option<ReferenceQueryIdentity>,
        message: String,
    },
    /// The motif of every intron of one [`SpliceMotifQuery`], in its order. An intron whose
    /// bases could not be read is `SpliceMotif::Unavailable`.
    SpliceMotifs {
        revision: ReferenceRevision,
        contig: ContigId,
        motifs: Vec<((u64, u64), SpliceMotif)>,
    },
}

/// One completed reference query, as handed to a [`ReferenceSliceSink`] on the reference thread.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceSliceDelivery {
    /// Identity of the query the slice answers, including the panel that asked.
    pub query: ReferenceQueryIdentity,
    /// The bases; the same allocation the UI receives in
    /// [`ReferenceEngineEvent::QueryCompleted`].
    pub slice: Arc<ReferenceSlice>,
}

/// A consumer that receives completed reference slices directly from the reference thread,
/// without a hop through the UI thread. Obtain one from the consumer (for an alignment scene
/// engine, [`crate::AlignmentSceneEngine::reference_slice_sink`]) and connect it once with
/// [`ReferenceEngine::connect_slice_sink`].
///
/// The sink is called on the reference actor's thread for every completed, still-current
/// query, before the UI is told about it. It must not block.
#[derive(Clone)]
pub struct ReferenceSliceSink {
    deliver: Arc<dyn Fn(&ReferenceSliceDelivery) -> bool + Send + Sync>,
}

impl ReferenceSliceSink {
    /// `deliver` returns `false` once its consumer is gone; the sink is then disconnected.
    #[must_use]
    pub fn new(deliver: impl Fn(&ReferenceSliceDelivery) -> bool + Send + Sync + 'static) -> Self {
        Self {
            deliver: Arc::new(deliver),
        }
    }
}

impl std::fmt::Debug for ReferenceSliceSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReferenceSliceSink")
            .finish_non_exhaustive()
    }
}

type ReferenceSliceSinks = Arc<Mutex<Vec<ReferenceSliceSink>>>;

/// Single-owner FASTA actor. Opening an index and fetching sequence never blocks the UI thread.
pub struct ReferenceEngine {
    input: Option<Sender<ReferenceEngineRequest>>,
    output: CoalescingReceiver<ReferenceEngineEvent>,
    worker: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    slice_sinks: ReferenceSliceSinks,
}

impl ReferenceEngine {
    #[must_use]
    pub fn spawn(queue_capacity: usize) -> Self {
        let capacity = queue_capacity.max(1);
        let (input_tx, input_rx) = bounded(capacity);
        let (output_tx, output_rx) = bounded_coalescing_to_ui(capacity);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let slice_sinks = ReferenceSliceSinks::default();
        let worker_sinks = Arc::clone(&slice_sinks);
        let worker = thread::Builder::new()
            .name("rgv-reference-actor".to_owned())
            .spawn(move || {
                run_thread_root(
                    || worker_loop(&input_rx, &output_tx, &worker_shutdown, &worker_sinks),
                    |message| {
                        tracing::error!(message, "reference actor panicked");
                        // `query: None` is this actor's terminal failure form: the reference is
                        // no longer usable and the owner retires the engine.
                        send_event(
                            &output_tx,
                            ReferenceEngineEvent::Error {
                                query: None,
                                message: format!(
                                    "the reference engine stopped after an internal error: \
                                     {message}"
                                ),
                            },
                        );
                    },
                );
            })
            .expect("failed to spawn RGV reference actor");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
            shutdown,
            slice_sinks,
        }
    }

    /// Connects a consumer that receives every completed reference slice directly from the
    /// reference thread (reference thread to consumer thread, with no UI hop), from the next
    /// completed query on. Connect each consumer once; it is disconnected by itself when it
    /// reports that it is gone, and all of them when this engine is retired or dropped.
    ///
    /// [`ReferenceEngineEvent::QueryCompleted`] is still sent to the UI for every query, so a
    /// caller that relays slices itself keeps working, with or without sinks connected.
    pub fn connect_slice_sink(&self, sink: ReferenceSliceSink) {
        self.slice_sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sink);
    }

    /// Sinks currently connected.
    #[must_use]
    pub fn slice_sink_count(&self) -> usize {
        self.slice_sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn try_open(&self, path: PathBuf) -> Result<(), EngineQueueError> {
        self.try_open_with_options(path, FastaOpenOptions::default())
    }

    pub fn try_open_with_options(
        &self,
        path: PathBuf,
        options: FastaOpenOptions,
    ) -> Result<(), EngineQueueError> {
        self.try_send(ReferenceEngineRequest::Open { path, options })
    }

    pub fn try_query(
        &self,
        query: ReferenceQueryIdentity,
        clock: GenerationClock,
        contig_name: String,
    ) -> Result<(), EngineQueueError> {
        self.try_send(ReferenceEngineRequest::Query {
            query,
            clock,
            contig_name,
        })
    }

    /// Queues one splice-motif lookup (at most [`MAX_SPLICE_MOTIF_QUERY_INTRONS`] introns; any
    /// beyond are dropped). The bases are read on the reference actor, never on the caller.
    pub fn try_query_splice_motifs(
        &self,
        mut query: SpliceMotifQuery,
    ) -> Result<(), EngineQueueError> {
        query.introns.truncate(MAX_SPLICE_MOTIF_QUERY_INTRONS);
        self.try_send(ReferenceEngineRequest::SpliceMotifs(query))
    }

    pub fn try_receive(&self) -> Result<Option<ReferenceEngineEvent>, EngineQueueError> {
        self.output
            .try_receive()
            .map_err(|_| EngineQueueError::Disconnected)
    }

    /// Stop accepting work and transfer the join obligation to the caller.
    /// Poll `is_finished` before joining during interactive replacement; final
    /// shutdown may join directly. In-flight IO still obeys its transport deadline.
    #[must_use]
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.shutdown.store(true, Ordering::Release);
        self.disconnect_slice_sinks();
        self.input.take();
        self.output.close();
        self.worker.take()
    }

    fn disconnect_slice_sinks(&self) {
        self.slice_sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn try_send(&self, request: ReferenceEngineRequest) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => EngineQueueError::Full,
            TrySendError::Disconnected(_) => EngineQueueError::Disconnected,
        })
    }
}

impl Drop for ReferenceEngine {
    /// Signals the actor exactly as `retire` does and returns without joining it: an engine
    /// dropped on a path which forgot to retire must not block that thread on in-flight I/O.
    /// A reaper thread waits for the actor instead.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.disconnect_slice_sinks();
        self.input.take();
        self.output.close();
        if let Some(worker) = self.worker.take() {
            drop(reap_worker(
                worker,
                "rgv-reference-reaper",
                "reference actor",
            ));
        }
    }
}

/// Hands a completed slice to every connected sink and forgets the sinks whose consumer is gone.
fn deliver_to_sinks(sinks: &ReferenceSliceSinks, delivery: &ReferenceSliceDelivery) {
    // Called outside the lock: a sink must not be able to stall `connect_slice_sink`.
    let connected = sinks.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if connected.is_empty() {
        return;
    }
    let gone = connected
        .iter()
        .filter(|sink| !(sink.deliver)(delivery))
        .map(|sink| Arc::as_ptr(&sink.deliver).cast::<()>())
        .collect::<Vec<_>>();
    if !gone.is_empty() {
        sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|sink| !gone.contains(&Arc::as_ptr(&sink.deliver).cast::<()>()));
    }
}

fn worker_loop(
    input: &Receiver<ReferenceEngineRequest>,
    output: &CoalescingSender<ReferenceEngineEvent>,
    shutdown: &Arc<AtomicBool>,
    slice_sinks: &ReferenceSliceSinks,
) {
    let mut reference: Option<(FastaReference, ReferenceRevision)> = None;
    while let Ok(request) = input.recv() {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let event = match request {
            ReferenceEngineRequest::Open { path, options } => {
                match FastaReference::open_with_options(path, &options) {
                    Ok(opened) => {
                        let dictionary = opened.reference_dictionary().clone();
                        let remote_io_stats = opened.remote_io_stats();
                        if let Some(revision) = next_reference_open_instance_revision() {
                            reference = Some((opened, revision));
                            ReferenceEngineEvent::Opened {
                                revision,
                                dictionary,
                                remote_io_stats,
                            }
                        } else {
                            ReferenceEngineEvent::Error {
                                query: None,
                                message: "reference open-instance revision authority is exhausted"
                                    .to_owned(),
                            }
                        }
                    }
                    Err(error) => ReferenceEngineEvent::Error {
                        query: None,
                        message: error.to_string(),
                    },
                }
            }
            ReferenceEngineRequest::SpliceMotifs(query) => {
                let Some((reference, revision)) = reference.as_ref() else {
                    continue;
                };
                if query.revision != *revision {
                    continue;
                }
                let motif_shutdown = Arc::clone(shutdown);
                let cancellation: RemoteCancellation =
                    Arc::new(move || !motif_shutdown.load(Ordering::Acquire));
                reference.set_remote_cancellation(Some(cancellation));
                let Some(motifs) = fetch_splice_motifs(reference, &query, shutdown) else {
                    continue;
                };
                ReferenceEngineEvent::SpliceMotifs {
                    revision: query.revision,
                    contig: query.contig,
                    motifs,
                }
            }
            ReferenceEngineRequest::Query {
                query,
                clock,
                contig_name,
            } => {
                let Some((reference, revision)) = reference.as_ref() else {
                    send_event(
                        output,
                        ReferenceEngineEvent::Error {
                            query: Some(query),
                            message: "reference FASTA is not open".to_owned(),
                        },
                    );
                    continue;
                };
                if query.revision != *revision {
                    continue;
                }
                let query_clock = clock.clone();
                let generation = query.request.generation();
                let query_shutdown = Arc::clone(shutdown);
                let cancellation: RemoteCancellation = Arc::new(move || {
                    !query_shutdown.load(Ordering::Acquire) && query_clock.is_current(generation)
                });
                reference.set_remote_cancellation(Some(cancellation));
                match fetch_reference_query(
                    reference,
                    &contig_name,
                    query.interval,
                    &clock,
                    query,
                    shutdown,
                ) {
                    ReferenceQueryFetch::Completed(slice)
                        if clock.is_current(query.request.generation()) =>
                    {
                        let slice = Arc::new(slice);
                        // Engine to engine first: by the time the UI hears of the slice, every
                        // connected consumer already has it queued.
                        deliver_to_sinks(
                            slice_sinks,
                            &ReferenceSliceDelivery {
                                query,
                                slice: Arc::clone(&slice),
                            },
                        );
                        ReferenceEngineEvent::QueryCompleted {
                            query,
                            slice,
                            remote_io_stats: reference.remote_io_stats(),
                        }
                    }
                    ReferenceQueryFetch::MissingContig
                        if clock.is_current(query.request.generation()) =>
                    {
                        ReferenceEngineEvent::Error {
                            query: Some(query),
                            message: format!(
                                "reference FASTA does not contain contig {contig_name}"
                            ),
                        }
                    }
                    ReferenceQueryFetch::Failed(message)
                        if clock.is_current(query.request.generation()) =>
                    {
                        ReferenceEngineEvent::Error {
                            query: Some(query),
                            message,
                        }
                    }
                    ReferenceQueryFetch::Completed(_)
                    | ReferenceQueryFetch::MissingContig
                    | ReferenceQueryFetch::Cancelled
                    | ReferenceQueryFetch::Failed(_) => continue,
                }
            }
        };
        send_event(output, event);
    }
}

/// Reads the first and last two bases of each intron. `None` when the actor is shutting down.
fn fetch_splice_motifs(
    reference: &FastaReference,
    query: &SpliceMotifQuery,
    shutdown: &AtomicBool,
) -> Option<Vec<((u64, u64), SpliceMotif)>> {
    let two_bases = |start: u64| -> Option<[u8; 2]> {
        let interval = GenomicInterval::new(query.contig, start, start.checked_add(2)?).ok()?;
        let slice = reference
            .fetch_slice_by_name(&query.contig_name, interval)
            .ok()
            .flatten()?;
        match slice.bases.as_slice() {
            [first, second] => Some([*first, *second]),
            _ => None,
        }
    };
    let mut motifs = Vec::with_capacity(query.introns.len());
    for (start, end) in query.introns.iter().copied() {
        if shutdown.load(Ordering::Acquire) {
            return None;
        }
        let motif = if end >= start.saturating_add(4) {
            match (two_bases(start), two_bases(end - 2)) {
                (Some(first), Some(last)) => SpliceMotif::classify(first, last),
                _ => SpliceMotif::Unavailable,
            }
        } else {
            SpliceMotif::NonCanonical
        };
        motifs.push(((start, end), motif));
    }
    Some(motifs)
}

enum ReferenceQueryFetch {
    Completed(ReferenceSlice),
    MissingContig,
    Cancelled,
    Failed(String),
}

fn fetch_reference_query(
    reference: &FastaReference,
    contig_name: &str,
    interval: GenomicInterval,
    clock: &GenerationClock,
    query: ReferenceQueryIdentity,
    shutdown: &AtomicBool,
) -> ReferenceQueryFetch {
    if interval.len() > MAX_REFERENCE_SLICE_BASES {
        return ReferenceQueryFetch::Failed(format!(
            "reference query of {} bases exceeds the {MAX_REFERENCE_SLICE_BASES}-base slice limit",
            interval.len()
        ));
    }
    let expected_bases = match usize::try_from(interval.len()) {
        Ok(expected) => expected,
        Err(_) => {
            return ReferenceQueryFetch::Failed(
                "reference query length exceeds addressable memory".to_owned(),
            );
        }
    };
    let mut bases = Vec::new();
    if let Err(error) = bases.try_reserve_exact(expected_bases) {
        return ReferenceQueryFetch::Failed(format!(
            "could not reserve memory for reference query: {error}"
        ));
    }

    let mut start = interval.start();
    while start < interval.end() {
        if shutdown.load(Ordering::Acquire) || !clock.is_current(query.request.generation()) {
            return ReferenceQueryFetch::Cancelled;
        }
        let end = start
            .saturating_add(MAX_REFERENCE_QUERY_BASES)
            .min(interval.end());
        let chunk = GenomicInterval::new(interval.contig(), start, end)
            .expect("a bounded reference-query chunk is non-empty");
        let slice = match reference.fetch_slice_by_name(contig_name, chunk) {
            Ok(Some(slice)) => slice,
            Ok(None) => return ReferenceQueryFetch::MissingContig,
            Err(_) if !clock.is_current(query.request.generation()) => {
                return ReferenceQueryFetch::Cancelled;
            }
            Err(error) => return ReferenceQueryFetch::Failed(error.to_string()),
        };
        let expected_chunk_bases = match usize::try_from(chunk.len()) {
            Ok(expected) => expected,
            Err(_) => {
                return ReferenceQueryFetch::Failed(
                    "reference query chunk length exceeds addressable memory".to_owned(),
                );
            }
        };
        if slice.interval != chunk || slice.bases.len() != expected_chunk_bases {
            return ReferenceQueryFetch::Failed(format!(
                "reference source returned {} bases for a {}-base query chunk",
                slice.bases.len(),
                chunk.len()
            ));
        }
        bases.extend_from_slice(&slice.bases);
        start = end;
    }

    if shutdown.load(Ordering::Acquire) || !clock.is_current(query.request.generation()) {
        return ReferenceQueryFetch::Cancelled;
    }
    if bases.len() != expected_bases {
        return ReferenceQueryFetch::Failed(format!(
            "reference source returned {} bases for a {}-base logical query",
            bases.len(),
            interval.len()
        ));
    }
    ReferenceQueryFetch::Completed(ReferenceSlice { interval, bases })
}

fn next_reference_open_instance_revision() -> Option<ReferenceRevision> {
    let sequence = NEXT_REFERENCE_OPEN_INSTANCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .ok()?;
    Some(ReferenceRevision::new(RevisionFingerprint::new(
        REFERENCE_OPEN_INSTANCE_DOMAIN,
        sequence,
    )))
}

fn send_event(output: &CoalescingSender<ReferenceEngineEvent>, event: ReferenceEngineEvent) {
    let result = if let ReferenceEngineEvent::QueryCompleted { query, .. } = &event {
        output.send_latest_for(query.request.panel_id().get(), event)
    } else {
        output.send_control(event)
    };
    if result.is_err() {
        tracing::debug!("reference event receiver closed");
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use rgv_core::{ContigId, PanelId, ViewportGeneration};

    use super::*;

    fn wait_for_event(engine: &ReferenceEngine) -> ReferenceEngineEvent {
        loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                return event;
            }
        }
    }

    #[test]
    fn connected_sinks_receive_each_completed_slice_and_leave_when_their_consumer_is_gone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let ReferenceEngineEvent::Opened { revision, .. } = wait_for_event(&engine) else {
            panic!("reference did not open");
        };

        let delivered = Arc::new(Mutex::new(Vec::<ReferenceSliceDelivery>::new()));
        let recording = {
            let delivered = Arc::clone(&delivered);
            ReferenceSliceSink::new(move |delivery| {
                delivered.lock().unwrap().push(delivery.clone());
                true
            })
        };
        // A consumer that goes away after its first slice.
        let once = {
            let accepted = AtomicBool::new(false);
            ReferenceSliceSink::new(move |_| !accepted.swap(true, Ordering::AcqRel))
        };
        engine.connect_slice_sink(recording);
        engine.connect_slice_sink(once);
        assert_eq!(engine.slice_sink_count(), 2);

        let clock = GenerationClock::default();
        let mut completions = Vec::new();
        for (panel, start) in [(7, 0), (8, 2), (9, 4)] {
            let request = PanelGeneration::new(PanelId::new(panel), ViewportGeneration::new(0));
            let interval = GenomicInterval::new(ContigId::new(0), start, start + 4).unwrap();
            let query = ReferenceQueryIdentity::new(request, revision, interval);
            engine
                .try_query(query, clock.clone(), "chr1".to_owned())
                .unwrap();
            let ReferenceEngineEvent::QueryCompleted {
                query: completed,
                slice,
                ..
            } = wait_for_event(&engine)
            else {
                panic!("reference query did not complete");
            };
            assert_eq!(completed, query);
            completions.push((query, slice));
            // By the time the UI hears of a slice, every sink has been handed it.
            assert_eq!(delivered.lock().unwrap().len(), completions.len());
        }
        // The sink got exactly what the UI got: same identity, same allocation.
        let delivered = delivered.lock().unwrap().clone();
        assert_eq!(delivered.len(), 3);
        for (delivery, (query, slice)) in delivered.iter().zip(&completions) {
            assert_eq!(delivery.query, *query);
            assert!(Arc::ptr_eq(&delivery.slice, slice));
        }
        assert_eq!(delivered[1].slice.bases, b"GTAC");
        // The consumer that reported itself gone at the second slice was disconnected then.
        assert_eq!(engine.slice_sink_count(), 1);

        // A query that fails is not delivered.
        let request = PanelGeneration::new(PanelId::new(7), ViewportGeneration::new(0));
        let interval = GenomicInterval::new(ContigId::new(0), 0, 4).unwrap();
        engine
            .try_query(
                ReferenceQueryIdentity::new(request, revision, interval),
                clock,
                "missing".to_owned(),
            )
            .unwrap();
        assert!(matches!(
            wait_for_event(&engine),
            ReferenceEngineEvent::Error { query: Some(_), .. }
        ));
        assert_eq!(delivered.len(), 3);
    }

    #[test]
    fn standalone_reference_opens_and_fetches_by_panel_generation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n>chr2\nTTAA\n").unwrap();
        fs::write(
            path.with_extension("fa.fai"),
            b"chr1\t8\t6\t8\t9\nchr2\t4\t21\t4\t5\n",
        )
        .unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let opened = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::Opened {
            revision: opened_revision,
            dictionary,
            ..
        } = opened
        else {
            panic!("reference did not open");
        };
        assert_eq!(dictionary.len(), 2);
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(7), ViewportGeneration::new(0));
        let interval = GenomicInterval::new(ContigId::new(0), 1, 5).unwrap();
        let query = ReferenceQueryIdentity::new(request, opened_revision, interval);
        engine.try_query(query, clock, "chr1".to_owned()).unwrap();
        let completed = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::QueryCompleted {
            query: completed_query,
            slice,
            ..
        } = completed
        else {
            panic!("reference query did not complete");
        };
        assert_eq!(completed_query, query);
        assert_eq!(slice.interval, interval);
        assert_eq!(slice.bases, b"CGTA");
    }

    #[test]
    fn splice_motifs_are_read_on_the_reference_actor_for_both_strands() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        // 0         1         2         3
        // 0123456789012345678901234567890123456789
        // AAGTCCCCAGAACTGGGGACAAAA
        let sequence = b"AAGTCCCCAGAACTGGGGACAAAAAAAATTTTTTTTTTTT";
        let mut fasta = b">chr1\n".to_vec();
        fasta.extend_from_slice(sequence);
        fasta.push(b'\n');
        fs::write(&path, &fasta).unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t40\t6\t40\t41\n").unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let ReferenceEngineEvent::Opened { revision, .. } = wait_for_event(&engine) else {
            panic!("reference did not open");
        };
        engine
            .try_query_splice_motifs(SpliceMotifQuery {
                revision,
                contig: ContigId::new(0),
                contig_name: "chr1".to_owned(),
                // GT..AG on +, CT..AC (GT-AG on -), a non-canonical intron, and one past the
                // contig end.
                introns: vec![(2, 10), (12, 20), (24, 32), (36, 48)],
            })
            .unwrap();
        let ReferenceEngineEvent::SpliceMotifs {
            revision: answered,
            contig,
            motifs,
        } = wait_for_event(&engine)
        else {
            panic!("splice motifs were not answered");
        };
        assert_eq!(answered, revision);
        assert_eq!(contig, ContigId::new(0));
        assert_eq!(
            motifs,
            vec![
                ((2, 10), SpliceMotif::GtAg),
                ((12, 20), SpliceMotif::CtAc),
                ((24, 32), SpliceMotif::NonCanonical),
                ((36, 48), SpliceMotif::Unavailable),
            ]
        );
    }

    #[test]
    fn reference_revision_is_stable_within_an_open_and_changes_on_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);

        engine.try_open(path.clone()).unwrap();
        let first_open = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::Opened {
            revision: first_revision,
            ..
        } = first_open
        else {
            panic!("first reference did not open");
        };

        let clock = GenerationClock::default();
        for panel in [11, 12] {
            let request = PanelGeneration::new(PanelId::new(panel), ViewportGeneration::new(0));
            let interval = GenomicInterval::new(ContigId::new(0), 0, 4).unwrap();
            let query = ReferenceQueryIdentity::new(request, first_revision, interval);
            engine
                .try_query(query, clock.clone(), "chr1".to_owned())
                .unwrap();
            let completed = loop {
                if let Some(event) = engine
                    .output
                    .receive_timeout(Duration::from_secs(2))
                    .unwrap()
                {
                    break event;
                }
            };
            let ReferenceEngineEvent::QueryCompleted {
                query: completed_query,
                ..
            } = completed
            else {
                panic!("reference query did not complete");
            };
            assert_eq!(completed_query, query);
        }

        engine.try_open(path).unwrap();
        let second_open = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::Opened {
            revision: second_revision,
            ..
        } = second_open
        else {
            panic!("second reference did not open");
        };
        assert_ne!(second_revision, first_revision);

        let request = PanelGeneration::new(PanelId::new(13), ViewportGeneration::new(0));
        let interval = GenomicInterval::new(ContigId::new(0), 4, 8).unwrap();
        let query = ReferenceQueryIdentity::new(request, second_revision, interval);
        engine.try_query(query, clock, "chr1".to_owned()).unwrap();
        let completed = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::QueryCompleted {
            query: completed_query,
            ..
        } = completed
        else {
            panic!("reopened reference query did not complete");
        };
        assert_eq!(completed_query, query);
    }

    #[test]
    fn query_for_an_obsolete_open_revision_is_silently_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);

        engine.try_open(path.clone()).unwrap();
        let first_revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };
        engine.try_open(path).unwrap();
        let second_revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };
        assert_ne!(first_revision, second_revision);

        let interval = GenomicInterval::new(ContigId::new(0), 0, 4).unwrap();
        let stale = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(21), ViewportGeneration::new(0)),
            first_revision,
            interval,
        );
        let current = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(22), ViewportGeneration::new(0)),
            second_revision,
            interval,
        );
        let clock = GenerationClock::default();
        engine
            .try_query(stale, clock.clone(), "chr1".to_owned())
            .unwrap();
        engine.try_query(current, clock, "chr1".to_owned()).unwrap();

        let completed = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            completed,
            ReferenceEngineEvent::QueryCompleted { query, .. } if query == current
        ));
        assert_eq!(
            engine
                .output
                .receive_timeout(Duration::from_millis(100))
                .unwrap(),
            None
        );
    }

    #[test]
    fn same_generation_query_errors_retain_revision_and_exact_interval_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };

        let request = PanelGeneration::new(PanelId::new(31), ViewportGeneration::new(0));
        let first = ReferenceQueryIdentity::new(
            request,
            revision,
            GenomicInterval::new(ContigId::new(0), 0, 9).unwrap(),
        );
        let second = ReferenceQueryIdentity::new(
            request,
            revision,
            GenomicInterval::new(ContigId::new(0), 1, 10).unwrap(),
        );
        let clock = GenerationClock::default();
        engine
            .try_query(first, clock.clone(), "chr1".to_owned())
            .unwrap();
        engine.try_query(second, clock, "chr1".to_owned()).unwrap();

        for expected in [first, second] {
            let event = loop {
                if let Some(event) = engine
                    .output
                    .receive_timeout(Duration::from_secs(2))
                    .unwrap()
                {
                    break event;
                }
            };
            assert!(matches!(
                event,
                ReferenceEngineEvent::Error {
                    query: Some(query),
                    ..
                } if query == expected
            ));
        }
    }

    #[test]
    fn a_query_wider_than_the_slice_limit_is_refused_before_any_allocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        fs::write(&path, b">chr1\nACGTACGT\n").unwrap();
        fs::write(path.with_extension("fa.fai"), b"chr1\t8\t6\t8\t9\n").unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };
        let clock = GenerationClock::default();
        let query = |end| {
            ReferenceQueryIdentity::new(
                PanelGeneration::new(PanelId::new(33), clock.current()),
                revision,
                GenomicInterval::new(ContigId::new(0), 0, end).unwrap(),
            )
        };
        // A corrupt or absurd interval must not be turned into a multi-terabyte reservation.
        let oversized = query(u64::MAX);
        let just_over = query(MAX_REFERENCE_SLICE_BASES + 1);
        for refused in [oversized, just_over] {
            engine
                .try_query(refused, clock.clone(), "chr1".to_owned())
                .unwrap();
            let event = loop {
                if let Some(event) = engine
                    .output
                    .receive_timeout(Duration::from_secs(2))
                    .unwrap()
                {
                    break event;
                }
            };
            let ReferenceEngineEvent::Error {
                query: Some(failed),
                message,
            } = event
            else {
                panic!("an oversized reference query must fail with its identity");
            };
            assert_eq!(failed, refused);
            assert!(
                message.contains("slice limit"),
                "unexpected message {message}"
            );
        }

        // The limit itself is not a behaviour change for ordinary queries.
        let ordinary = query(8);
        engine
            .try_query(ordinary, clock, "chr1".to_owned())
            .unwrap();
        let event = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            event,
            ReferenceEngineEvent::QueryCompleted { query, .. } if query == ordinary
        ));
    }

    #[test]
    fn logical_query_larger_than_one_fetch_chunk_completes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.fa");
        let sequence_len = usize::try_from(MAX_REFERENCE_QUERY_BASES + 2).unwrap();
        let mut fasta = Vec::with_capacity(sequence_len + 7);
        fasta.extend_from_slice(b">chr1\n");
        fasta.resize(fasta.len() + sequence_len, b'A');
        fasta.push(b'\n');
        fs::write(&path, fasta).unwrap();
        fs::write(
            path.with_extension("fa.fai"),
            format!(
                "chr1\t{sequence_len}\t6\t{sequence_len}\t{}\n",
                sequence_len + 1
            ),
        )
        .unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine.try_open(path).unwrap();
        let revision = loop {
            if let Some(ReferenceEngineEvent::Opened { revision, .. }) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break revision;
            }
        };

        let interval =
            GenomicInterval::new(ContigId::new(0), 0, u64::try_from(sequence_len).unwrap())
                .unwrap();
        let query = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(32), ViewportGeneration::new(0)),
            revision,
            interval,
        );
        engine
            .try_query(query, GenerationClock::default(), "chr1".to_owned())
            .unwrap();

        let event = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        let ReferenceEngineEvent::QueryCompleted {
            query: completed_query,
            slice,
            ..
        } = event
        else {
            panic!("chunked logical reference query did not complete");
        };
        assert_eq!(completed_query, query);
        assert_eq!(slice.interval, interval);
        assert_eq!(slice.bases.len(), sequence_len);
        assert!(slice.bases.iter().all(|base| *base == b'A'));
    }

    #[test]
    fn open_errors_remain_identity_less_while_query_errors_are_identified() {
        let directory = tempfile::tempdir().unwrap();
        let engine = ReferenceEngine::spawn(4);
        engine
            .try_open(directory.path().join("missing.fa"))
            .unwrap();
        let open_error = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            open_error,
            ReferenceEngineEvent::Error { query: None, .. }
        ));

        let query = ReferenceQueryIdentity::new(
            PanelGeneration::new(PanelId::new(41), ViewportGeneration::new(0)),
            ReferenceRevision::new(RevisionFingerprint::new(9, 10)),
            GenomicInterval::new(ContigId::new(0), 0, 1).unwrap(),
        );
        engine
            .try_query(query, GenerationClock::default(), "chr1".to_owned())
            .unwrap();
        let query_error = loop {
            if let Some(event) = engine
                .output
                .receive_timeout(Duration::from_secs(2))
                .unwrap()
            {
                break event;
            }
        };
        assert!(matches!(
            query_error,
            ReferenceEngineEvent::Error {
                query: Some(identity),
                ..
            } if identity == query
        ));
    }
}
