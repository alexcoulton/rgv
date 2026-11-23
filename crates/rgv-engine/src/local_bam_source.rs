use std::{
    collections::BTreeMap,
    mem::size_of,
    num::NonZeroUsize,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use rgv_alignment_scene::{
    AlignmentHandle, AlignmentSourceRevision, AllocationClass, AllocationLedger,
    CanonicalRecordChunk, CapabilityRequirement, ChunkError, CompleteCoverage, ContractFingerprint,
    CoverageError, EvidenceChunk, EvidenceChunkError, EvidenceStore, FilteredRecordProvenance,
    LedgerError, PayloadContract, PayloadSidecar, PayloadSidecarError, RecordContract,
    ReservationKind, SourceChunkId, TransientResourceClass,
};
use rgv_core::{GenomicInterval, OverlappingMatePolicy};
use rgv_hts::{
    AlignmentBatch, AlignmentError, AlignmentPayloadBatch, AlignmentPayloadError, AlignmentSource,
    IndividualReadStatus, PositionedAlignmentBlock, PositionedAlignmentStreamCheckpoint,
    PositionedChunkReceipt, ProductionAlignmentSource, QueryDetail, QueryOptions, QueryStats,
    RemoteTransferBudget,
};
use thiserror::Error;

use crate::alignment_scene_coordinator::budget_pressure;
use crate::{
    AlignmentSourceIntentSpec, CanonicalAlignmentSourceBackend, CanonicalDecodeCheckpoint,
    SourceIntentKind, SourceWork,
    decode_allowance::{DecodeAllowance, DecodeClass},
    mailbox::{MailboxWaker, panic_message},
};

/// Non-discardable controls which do not change physical-record eligibility. Filter or source
/// changes require a new actor/store because they change stable authority contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalBamSourceControl {
    SetCompleteReadLimit(NonZeroUsize),
    SetCancellationChunk(NonZeroUsize),
}

/// Production adapter from a resumable positioned decoder to canonical immutable evidence.
///
/// This backend serves every production alignment source: local BAM through `HTSlib`, and
/// local CRAM, remote BAM and remote CRAM through the positioned pure-Rust decoders. One
/// instance owns one mutable indexed reader. It never certifies a partial transaction and it
/// returns to the actor after every physical owner chunk (BGZF block or CRAM slice), including
/// chunks containing only filtered records.
///
/// It keeps the decoder's receipt of every chunk it handed over. A later stream whose target
/// overlaps chunks the evidence store still holds is started with those receipts, so the
/// decoder seeks past the chunks instead of reading them again (see
/// [`CanonicalAlignmentSourceBackend::begin_work_resuming`]).
pub struct PositionedAlignmentBackend {
    decoder: DecoderThread,
    is_cram: bool,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    options: QueryOptions,
    receipts: ReceiptBook,
}

/// Former name of [`PositionedAlignmentBackend`], from when it served local BAM only.
#[deprecated(
    note = "renamed to `PositionedAlignmentBackend`: it serves local and remote BAM and CRAM"
)]
pub type LocalBamCanonicalBackend = PositionedAlignmentBackend;

/// Most bytes of chunk receipts one backend keeps (about 16 bytes per record a stream saw).
/// Receipts are small next to the evidence they describe, which the allocation ledger bounds;
/// this caps them on their own so a long session over a filtered-out or evicted region cannot
/// grow them without limit. The oldest are dropped first, which only costs a later re-decode.
const RECEIPT_BYTE_LIMIT: usize = 8 << 20;

/// Receipts held between passes that drop the receipts of chunks nobody holds any more.
const RECEIPT_PRUNE_INTERVAL: u64 = 256;

/// What a backend's decode thread has done, and the chunk receipts the backend holds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentDecodeTelemetry {
    /// Streams the decode thread began.
    pub streams_started: u64,
    /// Source units (BGZF blocks, CRAM slices) the decode thread read, delivered or not.
    pub blocks_decoded: u64,
    /// Records retained in those units.
    pub records_decoded: u64,
    /// Receipts of resident chunks handed to the decoder when a stream began.
    pub receipts_offered: u64,
    /// Receipts the decoder honoured: chunks it did not read again.
    pub receipts_honoured: u64,
    /// Receipts currently held.
    pub receipts_held: usize,
    /// Bytes those receipts retain; never above the backend's receipt limit.
    pub receipt_bytes: usize,
}

#[derive(Debug, Default)]
struct DecodeCounters {
    streams_started: AtomicU64,
    blocks_decoded: AtomicU64,
    records_decoded: AtomicU64,
    receipts_offered: AtomicU64,
    receipts_honoured: AtomicU64,
}

/// File identity of an owner chunk as its receipt reports it: the block's compressed offset
/// and, for CRAM, the slice landmark.
type ReceiptKey = (u64, Option<u64>);

fn receipt_key(receipt: &PositionedChunkReceipt) -> ReceiptKey {
    (receipt.compressed_offset(), receipt.cram_slice_landmark())
}

struct HeldReceipt {
    receipt: PositionedChunkReceipt,
    /// The evidence chunk holding the records of the receipt's block. `None` for a block whose
    /// records were all filtered out: nothing has to be resident for it to be skipped.
    chunk: Option<Weak<EvidenceChunk>>,
    /// Smallest start and largest end of the chunk's records.
    span: (u64, u64),
    age: u64,
    bytes: usize,
}

/// Receipt of the chunk the last checkpoint handed to the actor, until the actor reports which
/// resident chunk holds its records.
struct PendingReceipt {
    receipt: PositionedChunkReceipt,
    id: SourceChunkId,
    records: usize,
    span: (u64, u64),
}

/// The receipts a backend holds, one per owner chunk (the latest stream's), within
/// [`RECEIPT_BYTE_LIMIT`]. A receipt is only ever offered to the decoder while the evidence
/// store still holds the exact chunk it was bound to; the weak reference lets go of the chunk
/// as soon as the store and every scene do.
#[derive(Default)]
struct ReceiptBook {
    held: BTreeMap<ReceiptKey, HeldReceipt>,
    bytes: usize,
    byte_limit: usize,
    next_age: u64,
    pending: Option<PendingReceipt>,
}

impl ReceiptBook {
    fn new(byte_limit: usize) -> Self {
        Self {
            byte_limit,
            ..Self::default()
        }
    }

    fn hold(
        &mut self,
        receipt: PositionedChunkReceipt,
        chunk: Option<Weak<EvidenceChunk>>,
        span: (u64, u64),
    ) {
        let bytes = receipt.estimated_bytes();
        if bytes > self.byte_limit {
            self.remove(receipt_key(&receipt));
            return;
        }
        let age = self.next_age;
        self.next_age += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        let replaced = self.held.insert(
            receipt_key(&receipt),
            HeldReceipt {
                receipt,
                chunk,
                span,
                age,
                bytes,
            },
        );
        if let Some(replaced) = replaced {
            self.bytes = self.bytes.saturating_sub(replaced.bytes);
        }
        if self.bytes > self.byte_limit {
            self.shrink();
        } else if age % RECEIPT_PRUNE_INTERVAL == RECEIPT_PRUNE_INTERVAL - 1 {
            // Keeps the book in step with the evidence that is actually resident, well
            // before the byte limit would force it.
            self.forget_released_chunks();
        }
    }

    fn remove(&mut self, key: ReceiptKey) {
        if let Some(removed) = self.held.remove(&key) {
            self.bytes = self.bytes.saturating_sub(removed.bytes);
        }
    }

    /// Drops the receipts of chunks that the store and every scene have let go of.
    fn forget_released_chunks(&mut self) {
        let dead = self
            .held
            .iter()
            .filter(|(_, held)| {
                held.chunk
                    .as_ref()
                    .is_some_and(|chunk| chunk.strong_count() == 0)
            })
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        for key in dead {
            self.remove(key);
        }
    }

    /// Drops receipts of chunks nobody holds any more, then the oldest, until three quarters
    /// of the limit is in use (so a full book is not trimmed again on every insertion).
    fn shrink(&mut self) {
        self.forget_released_chunks();
        let target = self.byte_limit / 4 * 3;
        if self.bytes <= target {
            return;
        }
        let mut by_age = self
            .held
            .iter()
            .map(|(key, held)| (held.age, *key))
            .collect::<Vec<_>>();
        by_age.sort_unstable();
        for (_, key) in by_age {
            if self.bytes <= target {
                break;
            }
            self.remove(key);
        }
    }

    fn clear(&mut self) {
        self.held.clear();
        self.bytes = 0;
        self.pending = None;
    }

    /// The receipts a stream over `target` may be started with, and the resident chunks that
    /// hold the skipped blocks' records. A chunk-bound receipt is returned only when `store`
    /// still holds that exact chunk with a payload satisfying `payload`: the records of an
    /// evicted chunk are gone, so its block has to be decoded again.
    fn resumable(
        &mut self,
        target: GenomicInterval,
        options: &QueryOptions,
        payload: &PayloadContract,
        store: &EvidenceStore,
    ) -> (Vec<PositionedChunkReceipt>, Vec<Arc<EvidenceChunk>>) {
        self.pending = None;
        let mut receipts = Vec::new();
        let mut chunks = Vec::new();
        let mut dead = Vec::new();
        for (key, held) in &self.held {
            let Some(bound) = &held.chunk else {
                if held.receipt.can_resume(target, options) {
                    receipts.push(held.receipt.clone());
                }
                continue;
            };
            let Some(chunk) = bound.upgrade() else {
                dead.push(*key);
                continue;
            };
            // A chunk with no record in the target contributes nothing to its acquisition;
            // leaving it out keeps the acquisition from pinning evidence it does not need.
            if held.span.0 >= target.end()
                || held.span.1 <= target.start()
                || !held.receipt.can_resume(target, options)
            {
                continue;
            }
            let resident = store
                .cached_chunk_fragment(chunk.chunk().id(), chunk.chunk().handles(), payload)
                .is_some_and(|cached| Arc::ptr_eq(cached, &chunk));
            if resident {
                receipts.push(held.receipt.clone());
                chunks.push(chunk);
            }
        }
        for key in dead {
            self.remove(key);
        }
        (receipts, chunks)
    }
}

/// Finished source units a decoder thread may hold ahead of the scene worker. Small on purpose:
/// decoded blocks waiting here are not yet charged to the allocation ledger.
const DECODE_READ_AHEAD: usize = 2;

/// Most a single remote acquisition may transfer before it fails with a clear error instead
/// of downloading indefinitely. Local sources ignore it.
const REMOTE_STREAM_TRANSFER_BUDGET: RemoteTransferBudget = RemoteTransferBudget {
    maximum_requests: 20_000,
    maximum_response_bytes: 2 << 30,
};

struct DecodeRequest {
    generation: u64,
    target: GenomicInterval,
    admission_target: GenomicInterval,
    options: QueryOptions,
    /// Receipts of chunks whose records the caller still holds; the stream skips them.
    skip: Vec<PositionedChunkReceipt>,
    class: DecodeClass,
}

struct DecodeResult {
    generation: u64,
    checkpoint: Result<PositionedAlignmentStreamCheckpoint, DecodeFailure>,
    /// Receipt of the block in `checkpoint`, when the decoder issued one.
    receipt: Option<PositionedChunkReceipt>,
}

/// One finished unit of a stream with the receipt of its block, if it is a block.
type DecodedUnit = (
    PositionedAlignmentStreamCheckpoint,
    Option<PositionedChunkReceipt>,
);

enum DecodeFailure {
    Source(AlignmentError),
    /// The decoder panicked inside this stream. The thread survived and serves the next one.
    Panicked(String),
}

impl From<DecodeFailure> for LocalBamCanonicalBackendError {
    fn from(failure: DecodeFailure) -> Self {
        match failure {
            DecodeFailure::Source(error) => Self::Alignment(error),
            DecodeFailure::Panicked(message) => Self::DecoderPanicked(message),
        }
    }
}

/// The positioned decoder the decode thread drives. Production uses
/// [`ProductionAlignmentSource`]; the seam exists so thread-level behaviour (cancellation,
/// panic containment, wake-ups) can be exercised without a real file.
pub(crate) trait PositionedDecodeSource: Send + 'static {
    fn prepare_stream(&mut self, still_wanted: Arc<dyn Fn() -> bool + Send + Sync>);

    /// Starts a stream that does not decode the chunks named by `skip` again, and returns how
    /// many of those receipts it honoured. A source that cannot resume ignores them all.
    fn begin_stream(
        &mut self,
        target: GenomicInterval,
        admission_target: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<usize, AlignmentError>;

    fn next_checkpoint(
        &mut self,
        still_wanted: &dyn Fn() -> bool,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError>;

    /// The receipt of the block the last checkpoint returned, once.
    fn take_chunk_receipt(&mut self) -> Option<PositionedChunkReceipt> {
        None
    }

    fn cancel_stream(&mut self);
}

impl PositionedDecodeSource for ProductionAlignmentSource {
    fn prepare_stream(&mut self, still_wanted: Arc<dyn Fn() -> bool + Send + Sync>) {
        self.set_remote_cancellation(Some(still_wanted));
        // A fresh allowance per stream: one view may not download without limit, however the
        // index and the file disagree.
        self.set_remote_transfer_budget(Some(REMOTE_STREAM_TRANSFER_BUDGET));
    }

    fn begin_stream(
        &mut self,
        target: GenomicInterval,
        admission_target: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<usize, AlignmentError> {
        self.begin_positioned_stream_resuming(target, admission_target, options, skip)
    }

    fn next_checkpoint(
        &mut self,
        still_wanted: &dyn Fn() -> bool,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
        self.next_positioned_stream_checkpoint(still_wanted)
    }

    fn take_chunk_receipt(&mut self) -> Option<PositionedChunkReceipt> {
        self.take_positioned_chunk_receipt()
    }

    fn cancel_stream(&mut self) {
        self.cancel_positioned_stream();
    }
}

/// Owns the mutable indexed reader on a dedicated thread, so file and network reads never
/// block the scene worker. Each stream has a generation; raising the shared generation cancels
/// the stream in flight, including inside a block and inside a remote transfer.
struct DecoderThread {
    requests: Option<mpsc::Sender<DecodeRequest>>,
    results: Receiver<DecodeResult>,
    /// Generation of the stream the backend currently wants; zero means none.
    wanted: Arc<AtomicU64>,
    /// Generation whose results `next` accepts; `None` after a terminal result or a cancel.
    active: Option<u64>,
    last_generation: u64,
    /// Whether `next` waits for the decoder. Synchronous drivers wait; the threaded scene
    /// worker does not, so it can build presentation and observe newer targets meanwhile.
    wait_for_units: bool,
    /// Called by the decode thread after every hand-over, so a non-waiting driver can sleep
    /// until a unit is ready instead of polling `next`.
    unit_ready: Arc<OnceLock<MailboxWaker>>,
    /// Bounds how many decode threads of the process read a source unit at the same time.
    allowance: Arc<DecodeAllowance>,
    counters: Arc<DecodeCounters>,
    worker: Option<JoinHandle<()>>,
}

impl DecoderThread {
    fn spawn(source: impl PositionedDecodeSource) -> Self {
        Self::spawn_with_allowance(source, DecodeAllowance::process())
    }

    fn spawn_with_allowance(
        source: impl PositionedDecodeSource,
        allowance: Arc<DecodeAllowance>,
    ) -> Self {
        let (requests, request_rx) = mpsc::channel::<DecodeRequest>();
        let (result_tx, results) = mpsc::sync_channel::<DecodeResult>(DECODE_READ_AHEAD);
        let wanted = Arc::new(AtomicU64::new(0));
        let thread_wanted = Arc::clone(&wanted);
        let unit_ready = Arc::new(OnceLock::new());
        let thread_unit_ready = Arc::clone(&unit_ready);
        let counters = Arc::new(DecodeCounters::default());
        let thread_counters = Arc::clone(&counters);
        let thread_allowance = Arc::clone(&allowance);
        let worker = thread::Builder::new()
            .name("rgv-alignment-decode".to_owned())
            .spawn(move || {
                decode_loop(
                    source,
                    &request_rx,
                    &result_tx,
                    &thread_wanted,
                    &thread_unit_ready,
                    &thread_allowance,
                    &thread_counters,
                );
            })
            .expect("failed to spawn RGV alignment decode thread");
        Self {
            requests: Some(requests),
            results,
            wanted,
            active: None,
            last_generation: 0,
            wait_for_units: true,
            unit_ready,
            allowance,
            counters,
            worker: Some(worker),
        }
    }

    /// Starts a fresh foreground stream.
    #[cfg(test)]
    fn begin(
        &mut self,
        target: GenomicInterval,
        admission_target: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), LocalBamCanonicalBackendError> {
        self.begin_resuming(
            target,
            admission_target,
            options,
            Vec::new(),
            DecodeClass::Foreground,
        )
    }

    /// Starts a stream that skips the chunks named by `skip`, superseding the one in flight.
    fn begin_resuming(
        &mut self,
        target: GenomicInterval,
        admission_target: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
        class: DecodeClass,
    ) -> Result<(), LocalBamCanonicalBackendError> {
        self.last_generation += 1;
        let generation = self.last_generation;
        self.wanted.store(generation, Ordering::Release);
        self.active = Some(generation);
        // A superseded stream waiting for its turn to decode leaves the line.
        self.allowance.wake();
        // Results of superseded streams are dropped here; this also unblocks a decoder that
        // was waiting to hand over a block nobody wants any more.
        while self.results.try_recv().is_ok() {}
        self.requests
            .as_ref()
            .and_then(|requests| {
                requests
                    .send(DecodeRequest {
                        generation,
                        target,
                        admission_target,
                        options,
                        skip,
                        class,
                    })
                    .ok()
            })
            .ok_or(LocalBamCanonicalBackendError::DecoderStopped)
    }

    fn cancel(&mut self) {
        self.active = None;
        self.wanted.store(0, Ordering::Release);
        self.allowance.wake();
        while self.results.try_recv().is_ok() {}
    }

    /// The next finished unit of the active stream, or `None` when the decoder is still busy.
    fn next(&mut self) -> Result<Option<DecodedUnit>, LocalBamCanonicalBackendError> {
        let Some(active) = self.active else {
            return Err(LocalBamCanonicalBackendError::NoActiveStream);
        };
        loop {
            let received = if self.wait_for_units {
                self.results.recv().map_err(|_| TryRecvError::Disconnected)
            } else {
                self.results.try_recv()
            };
            match received {
                Ok(result) if result.generation == active => {
                    let checkpoint = result.checkpoint.inspect_err(|_| self.active = None)?;
                    if !matches!(checkpoint, PositionedAlignmentStreamCheckpoint::Block(_)) {
                        self.active = None;
                    }
                    return Ok(Some((checkpoint, result.receipt)));
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => {
                    return Err(LocalBamCanonicalBackendError::DecoderStopped);
                }
            }
        }
    }
}

impl Drop for DecoderThread {
    fn drop(&mut self) {
        self.wanted.store(0, Ordering::Release);
        self.requests = None;
        self.allowance.wake();
        // Unblock a decoder waiting on a full hand-over queue, then let it finish on its own;
        // joining here could wait on a slow remote read.
        while self.results.try_recv().is_ok() {}
        drop(self.worker.take());
    }
}

/// Thread root of the decoder. Each stream runs inside `catch_unwind`: a panic in the reader
/// becomes that stream's terminal error, carrying the panic text, and the thread goes on to
/// serve the next `begin`. A decoder is therefore never lost to one bad read.
fn decode_loop<S: PositionedDecodeSource>(
    mut source: S,
    requests: &Receiver<DecodeRequest>,
    results: &SyncSender<DecodeResult>,
    wanted: &Arc<AtomicU64>,
    unit_ready: &OnceLock<MailboxWaker>,
    allowance: &DecodeAllowance,
    counters: &DecodeCounters,
) {
    while let Ok(mut request) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            request = newer;
        }
        let generation = request.generation;
        let still_wanted = {
            let wanted = Arc::clone(wanted);
            move || wanted.load(Ordering::Acquire) == generation
        };
        if !still_wanted() {
            continue;
        }
        // Hands one finished unit (or the stream's terminal result) to the backend and wakes a
        // sleeping driver. `false` means the backend is gone.
        let deliver = |checkpoint, receipt| {
            let delivered = results
                .send(DecodeResult {
                    generation,
                    checkpoint,
                    receipt,
                })
                .is_ok();
            if let Some(wake) = unit_ready.get() {
                wake();
            }
            delivered
        };
        let stream = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_stream(
                &mut source,
                request,
                &still_wanted,
                &deliver,
                allowance,
                counters,
            )
        }));
        match stream {
            Ok(true) => {}
            Ok(false) => return,
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                tracing::error!(
                    message,
                    "alignment decoder panicked; the stream was abandoned"
                );
                // Best effort: leave the reader without a half-consumed stream. If this panics
                // too, the next stream's own containment reports it.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.cancel_stream();
                }));
                if still_wanted() && !deliver(Err(DecodeFailure::Panicked(message)), None) {
                    return;
                }
            }
        }
    }
}

/// Decodes one stream to its terminal result. Returns `false` once the backend is gone.
///
/// Each source read (the stream start and every unit) is made under a permit of `allowance`,
/// taken just before the read and given back before the unit is handed over, so a decoder
/// waiting for its consumer never keeps another source from decoding.
fn decode_stream<S: PositionedDecodeSource>(
    source: &mut S,
    request: DecodeRequest,
    still_wanted: &(impl Fn() -> bool + Clone + Send + Sync + 'static),
    deliver: &impl Fn(
        Result<PositionedAlignmentStreamCheckpoint, DecodeFailure>,
        Option<PositionedChunkReceipt>,
    ) -> bool,
    allowance: &DecodeAllowance,
    counters: &DecodeCounters,
) -> bool {
    source.prepare_stream(Arc::new(still_wanted.clone()));
    let class = request.class;
    let offered = request.skip.len() as u64;
    let begun = {
        let Some(_permit) = allowance.acquire(class, still_wanted) else {
            return true;
        };
        source.begin_stream(
            request.target,
            request.admission_target,
            request.options,
            request.skip,
        )
    };
    match begun {
        Ok(honoured) => {
            counters.streams_started.fetch_add(1, Ordering::Relaxed);
            counters
                .receipts_offered
                .fetch_add(offered, Ordering::Relaxed);
            counters
                .receipts_honoured
                .fetch_add(honoured as u64, Ordering::Relaxed);
        }
        Err(error) => {
            return !still_wanted() || deliver(Err(DecodeFailure::Source(error)), None);
        }
    }
    loop {
        if !still_wanted() {
            source.cancel_stream();
            return true;
        }
        let (checkpoint, receipt) = {
            let Some(_permit) = allowance.acquire(class, still_wanted) else {
                source.cancel_stream();
                return true;
            };
            let checkpoint = source.next_checkpoint(still_wanted);
            let receipt = match &checkpoint {
                Ok(PositionedAlignmentStreamCheckpoint::Block(block)) => {
                    counters.blocks_decoded.fetch_add(1, Ordering::Relaxed);
                    counters
                        .records_decoded
                        .fetch_add(block.records.len() as u64, Ordering::Relaxed);
                    source.take_chunk_receipt()
                }
                _ => None,
            };
            (checkpoint, receipt)
        };
        if !still_wanted() {
            // Cancelled inside the unit; whatever came back belongs to nobody.
            source.cancel_stream();
            return true;
        }
        let finished = !matches!(
            checkpoint,
            Ok(PositionedAlignmentStreamCheckpoint::Block(_))
        );
        if !deliver(checkpoint.map_err(DecodeFailure::Source), receipt) {
            return false;
        }
        if finished {
            return true;
        }
    }
}

impl PositionedAlignmentBackend {
    pub fn new(
        source: impl Into<ProductionAlignmentSource>,
        options: QueryOptions,
    ) -> Result<Self, LocalBamCanonicalBackendError> {
        Self::with_decode_source(source.into(), options, |source| source)
    }

    /// Builds the backend around `wrap(source)`, so a test can observe or gate what the decode
    /// thread asks of the production decoder.
    pub(crate) fn with_decode_source<S: PositionedDecodeSource>(
        source: ProductionAlignmentSource,
        mut options: QueryOptions,
        wrap: impl FnOnce(ProductionAlignmentSource) -> S,
    ) -> Result<Self, LocalBamCanonicalBackendError> {
        let stamp = source
            .source_stamp()
            .ok_or(LocalBamCanonicalBackendError::MissingSourceRevision)?;
        let source_revision = AlignmentSourceRevision::from(stamp);
        let record_contract = record_contract_fingerprint(&options);

        // Exact summaries are scene-owned independent products. Source evidence decoding only
        // retains the payload requested by the active intent.
        options.collect_alleles = false;
        options.collect_modifications = false;
        options.overlapping_mate_policy = OverlappingMatePolicy::CountBoth;
        options.include_reference = false;
        Ok(Self {
            is_cram: source.is_cram(),
            decoder: DecoderThread::spawn(wrap(source)),
            source_revision,
            record_contract,
            options,
            receipts: ReceiptBook::new(RECEIPT_BYTE_LIMIT),
        })
    }

    /// What the decode thread has done so far and the chunk receipts held for later streams.
    #[must_use]
    pub fn decode_telemetry(&self) -> AlignmentDecodeTelemetry {
        let counters = &self.decoder.counters;
        AlignmentDecodeTelemetry {
            streams_started: counters.streams_started.load(Ordering::Relaxed),
            blocks_decoded: counters.blocks_decoded.load(Ordering::Relaxed),
            records_decoded: counters.records_decoded.load(Ordering::Relaxed),
            receipts_offered: counters.receipts_offered.load(Ordering::Relaxed),
            receipts_honoured: counters.receipts_honoured.load(Ordering::Relaxed),
            receipts_held: self.receipts.held.len(),
            receipt_bytes: self.receipts.bytes,
        }
    }

    /// Makes [`CanonicalAlignmentSourceBackend::next_checkpoint`] return
    /// [`CanonicalDecodeCheckpoint::Pending`] instead of waiting while the decode thread is
    /// busy. The threaded scene engine uses this; a driver that pumps synchronously keeps the
    /// default and waits for each unit.
    #[must_use]
    pub fn with_nonblocking_checkpoints(mut self) -> Self {
        self.decoder.wait_for_units = false;
        self
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn record_contract(&self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub fn intent_spec(
        &self,
        target: GenomicInterval,
        payload: PayloadContract,
    ) -> AlignmentSourceIntentSpec {
        AlignmentSourceIntentSpec::new(self.source_revision, self.record_contract, payload, target)
    }

    fn validate_work(&self, work: &SourceWork) -> Result<(), LocalBamCanonicalBackendError> {
        if work.intent().spec().source_revision() != self.source_revision {
            return Err(LocalBamCanonicalBackendError::SourceRevisionMismatch);
        }
        if work.intent().spec().record_contract() != self.record_contract {
            return Err(LocalBamCanonicalBackendError::RecordContractMismatch);
        }
        validate_payload_contract(work.intent().spec().payload())
    }

    fn options_for(
        &self,
        payload: &PayloadContract,
    ) -> Result<QueryOptions, LocalBamCanonicalBackendError> {
        validate_payload_contract(payload)?;
        let retain_sequence = payload.sequence() == CapabilityRequirement::Required;
        let mut options = self.options.clone();
        options.detail = if payload.all_auxiliary_tags() {
            QueryDetail::Full
        } else if retain_sequence {
            QueryDetail::Sequence
        } else {
            QueryDetail::Layout
        };
        options.layout_requires_sequence = retain_sequence;
        options.retained_auxiliary_tags = payload.auxiliary_tags().to_vec();
        Ok(options)
    }

    /// Starts the stream of `work`. With a `store`, the stream skips every chunk whose receipt
    /// is held, valid for the new target, and whose records `store` still holds with a payload
    /// that satisfies the work's; those chunks are returned so the caller can make them part
    /// of the transaction. Without one the stream is fresh.
    fn begin_or_retarget(
        &mut self,
        work: &SourceWork,
        store: Option<&EvidenceStore>,
    ) -> Result<Vec<Arc<EvidenceChunk>>, LocalBamCanonicalBackendError> {
        self.receipts.pending = None;
        self.validate_work(work)?;
        if self.is_cram {
            // CRAM decoding is reference-based; the decode reference is fixed when the source
            // is opened, so a payload may name it without changing what is decoded.
        } else if work.intent().spec().payload().decode_reference().is_some() {
            return Err(LocalBamCanonicalBackendError::DecodeReferenceUnsupportedForBam);
        }
        let spec = work.intent().spec();
        let options = self.options_for(spec.payload())?;
        let (skip, resident) = match store {
            Some(store) => self
                .receipts
                .resumable(spec.target(), &options, spec.payload(), store),
            None => (Vec::new(), Vec::new()),
        };
        self.decoder.begin_resuming(
            spec.target(),
            spec.admission_target(),
            options,
            skip,
            match work.kind() {
                SourceIntentKind::Foreground => DecodeClass::Foreground,
                SourceIntentKind::Speculative => DecodeClass::Speculative,
            },
        )?;
        Ok(resident)
    }

    fn pack_block(
        &self,
        work: &SourceWork,
        block: PositionedAlignmentBlock,
        ledger: &AllocationLedger,
    ) -> Result<Arc<EvidenceChunk>, LocalBamCanonicalBackendError> {
        let chunk_id = block
            .records
            .first()
            .map(|record| SourceChunkId::for_position(record.source_position))
            .ok_or(LocalBamCanonicalBackendError::EmptyOwnerChunk)?;
        let mut decoded = AlignmentBatch::default();
        for record in block.records {
            decoded
                .push_with_source_position(&record.record, record.source_position)
                .map_err(AlignmentError::BatchOverflow)?;
        }

        // The decoded batch is bounded decoder scratch. Before creating any retained scene
        // allocation, reserve conservative maxima for core, payload, and overlap storage. Core
        // and payload each retain a strict subset of the decoded packed columns.
        let decoded_bytes = decoded.estimated_bytes();
        let core_maximum = decoded_bytes
            .saturating_add(decoded.len().saturating_mul(size_of::<AlignmentHandle>()))
            .saturating_mul(2);
        let mut core_reservation = ledger.reserve(
            AllocationClass::CoreChunk,
            core_maximum,
            ReservationKind::Normal,
        )?;
        let overlap_maximum = decoded.len().saturating_mul(size_of::<u64>());
        let mut overlap_reservation = ledger.reserve(
            AllocationClass::OverlapIndex,
            overlap_maximum,
            ReservationKind::Normal,
        )?;
        let payload_contract = work.intent().spec().payload();
        let retain_payload = payload_is_required(payload_contract);
        let mut payload_reservation = retain_payload
            .then(|| {
                ledger.reserve(
                    AllocationClass::PayloadSidecar,
                    decoded_bytes.saturating_mul(2),
                    ReservationKind::Normal,
                )
            })
            .transpose()?;

        let core = CanonicalRecordChunk::new(
            self.source_revision,
            chunk_id,
            Arc::new(decoded.copy_core().map_err(AlignmentError::BatchOverflow)?),
            self.record_contract,
        )?;
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = core_reservation.register(footprint.core_bytes, work.id().get())?;
        let overlap_allocation =
            overlap_reservation.register(footprint.overlap_index_bytes, work.id().get())?;

        if let Some(payload_reservation) = payload_reservation.as_mut() {
            let payload = Arc::new(AlignmentPayloadBatch::from_batch(
                &decoded,
                payload_contract.sequence() == CapabilityRequirement::Required,
                payload_contract.qualities() == CapabilityRequirement::Required,
                (!payload_contract.all_auxiliary_tags())
                    .then_some(payload_contract.auxiliary_tags()),
            )?);
            let payload_bytes = payload.estimated_bytes();
            let payload_allocation =
                payload_reservation.register(payload_bytes, work.id().get())?;
            let payload = Arc::new(PayloadSidecar::new(
                &core,
                payload_contract.clone(),
                payload,
                payload_allocation,
            )?);
            Ok(Arc::new(EvidenceChunk::new_with_payload(
                core,
                core_allocation,
                overlap_allocation,
                payload,
            )?))
        } else {
            Ok(Arc::new(EvidenceChunk::new(
                core,
                core_allocation,
                overlap_allocation,
            )?))
        }
    }

    fn coverage(
        &self,
        work: &SourceWork,
        interval: GenomicInterval,
        stats: QueryStats,
    ) -> Result<CompleteCoverage, LocalBamCanonicalBackendError> {
        if interval != work.intent().spec().target() {
            return Err(LocalBamCanonicalBackendError::CompletionTargetMismatch {
                expected: work.intent().spec().target(),
                actual: interval,
            });
        }
        let provenance = FilteredRecordProvenance {
            records_seen: count_to_u64(stats.records_seen)?,
            eligible_records: count_to_u64(stats.records_eligible)?,
            unmapped: count_to_u64(stats.records_filtered_unmapped)?,
            secondary: count_to_u64(stats.records_filtered_secondary)?,
            supplementary: count_to_u64(stats.records_filtered_supplementary)?,
            user_filter: count_to_u64(stats.records_filtered_user)?,
        };
        Ok(CompleteCoverage::new(
            self.source_revision,
            self.record_contract,
            interval,
            provenance,
        )?)
    }
}

impl CanonicalAlignmentSourceBackend for PositionedAlignmentBackend {
    type Control = LocalBamSourceControl;
    type Error = LocalBamCanonicalBackendError;

    fn allocation_pressure(error: &Self::Error) -> Option<LedgerError> {
        match error {
            LocalBamCanonicalBackendError::Ledger(pressure) => budget_pressure(pressure),
            _ => None,
        }
    }

    fn control_invalidates_read_refusals(control: &Self::Control) -> bool {
        matches!(control, LocalBamSourceControl::SetCompleteReadLimit(_))
    }

    fn apply_control(&mut self, control: Self::Control) -> Result<(), Self::Error> {
        match control {
            LocalBamSourceControl::SetCompleteReadLimit(limit) => {
                self.options.max_records = limit;
            }
            LocalBamSourceControl::SetCancellationChunk(chunk) => {
                self.options.cancellation_chunk = chunk;
            }
        }
        Ok(())
    }

    fn begin_work(
        &mut self,
        work: &SourceWork,
        _ledger: &AllocationLedger,
    ) -> Result<(), Self::Error> {
        self.begin_or_retarget(work, None).map(drop)
    }

    fn begin_work_resuming(
        &mut self,
        work: &SourceWork,
        store: &EvidenceStore,
    ) -> Result<Vec<Arc<EvidenceChunk>>, Self::Error> {
        self.begin_or_retarget(work, Some(store))
    }

    fn retarget_resuming(
        &mut self,
        work: &SourceWork,
        store: &EvidenceStore,
    ) -> Result<Vec<Arc<EvidenceChunk>>, Self::Error> {
        self.begin_or_retarget(work, Some(store))
    }

    fn chunk_admitted(&mut self, _work: &SourceWork, chunk: &Arc<EvidenceChunk>) {
        let Some(pending) = self.receipts.pending.take() else {
            return;
        };
        // The store answers an admission with the chunk it keeps: the one just packed, or a
        // resident one holding the same records. Either way the receipt describes it.
        if chunk.chunk().id() == pending.id && chunk.chunk().handles().len() == pending.records {
            self.receipts
                .hold(pending.receipt, Some(Arc::downgrade(chunk)), pending.span);
        }
    }

    fn forget_resumable_chunks(&mut self) {
        self.receipts.clear();
    }

    fn next_checkpoint(
        &mut self,
        work: &SourceWork,
        ledger: &AllocationLedger,
    ) -> Result<CanonicalDecodeCheckpoint, Self::Error> {
        self.validate_work(work)?;
        let Some((checkpoint, receipt)) = self.decoder.next()? else {
            return Ok(CanonicalDecodeCheckpoint::Pending);
        };
        self.receipts.pending = None;
        match checkpoint {
            PositionedAlignmentStreamCheckpoint::Block(block) => {
                let genomic_span = block
                    .genomic_span
                    .unwrap_or_else(|| work.intent().spec().target());
                if block.records.is_empty() {
                    // Nothing was retained, so nothing has to stay resident for a later
                    // stream to skip this block.
                    if let Some(receipt) = receipt {
                        self.receipts.hold(receipt, None, (0, 0));
                    }
                    Ok(CanonicalDecodeCheckpoint::Progress { genomic_span })
                } else {
                    let records = block.records.len();
                    let span =
                        block
                            .records
                            .iter()
                            .fold((u64::MAX, 0), |(start, end), positioned| {
                                let record = &positioned.record;
                                (
                                    start.min(record.start),
                                    // A record consuming no reference base occupies one.
                                    end.max(record.end.max(record.start.saturating_add(1))),
                                )
                            });
                    // The returned normalized owner block remains live while we create packed
                    // core, overlap, and optional payload derivatives. Reserve their concurrent
                    // logical capacities before any of those duplicate allocations. HTSlib's
                    // opaque internal decode buffers are reported separately from this owned
                    // Rust working set.
                    let _decoder_scratch = ledger.reserve_transient(
                        TransientResourceClass::DecoderScratch,
                        block.estimated_bytes().saturating_mul(6),
                        ReservationKind::Normal,
                    )?;
                    let chunk = self.pack_block(work, block, ledger)?;
                    // Bound to the chunk the store keeps once the actor reports its admission.
                    self.receipts.pending = receipt.map(|receipt| PendingReceipt {
                        receipt,
                        id: chunk.chunk().id(),
                        records,
                        span,
                    });
                    Ok(CanonicalDecodeCheckpoint::Chunk {
                        chunk,
                        genomic_span,
                    })
                }
            }
            PositionedAlignmentStreamCheckpoint::Complete(completion) => {
                let status = completion.stats.individual_reads;
                let limit = completion.stats.complete_read_limit;
                let eligible_records = completion.stats.complete_read_records_eligible;
                let coverage = self.coverage(work, completion.interval, completion.stats)?;
                match status {
                    IndividualReadStatus::Complete => {
                        Ok(CanonicalDecodeCheckpoint::Complete(coverage))
                    }
                    IndividualReadStatus::TooDense { .. } => {
                        Ok(CanonicalDecodeCheckpoint::Refused {
                            coverage,
                            complete_read_limit: limit,
                            eligible_records,
                        })
                    }
                }
            }
            PositionedAlignmentStreamCheckpoint::Cancelled(_) => {
                Err(LocalBamCanonicalBackendError::UnexpectedDecoderCancellation)
            }
        }
    }

    fn retarget(&mut self, work: &SourceWork) -> Result<(), Self::Error> {
        self.begin_or_retarget(work, None).map(drop)
    }

    fn cancel(&mut self, _work: &SourceWork) {
        self.receipts.pending = None;
        self.decoder.cancel();
    }

    fn set_checkpoint_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) -> bool {
        // Only the first driver's waker is kept; one backend has one driver.
        self.decoder.unit_ready.set(waker).is_ok()
    }
}

fn payload_is_required(payload: &PayloadContract) -> bool {
    payload.sequence() == CapabilityRequirement::Required
        || payload.qualities() == CapabilityRequirement::Required
        || payload.all_auxiliary_tags()
        || !payload.auxiliary_tags().is_empty()
        || payload.decode_reference().is_some()
}

fn validate_payload_contract(
    payload: &PayloadContract,
) -> Result<(), LocalBamCanonicalBackendError> {
    if payload.qualities() == CapabilityRequirement::Required
        && payload.sequence() != CapabilityRequirement::Required
    {
        return Err(LocalBamCanonicalBackendError::QualitiesRequireSequence);
    }
    Ok(())
}

fn record_contract_fingerprint(options: &QueryOptions) -> ContractFingerprint {
    RecordContract::new(
        ContractFingerprint::new(options.filter.fingerprint()),
        options.include_secondary,
        options.include_supplementary,
    )
    .fingerprint()
}

fn count_to_u64(value: usize) -> Result<u64, LocalBamCanonicalBackendError> {
    u64::try_from(value).map_err(|_| LocalBamCanonicalBackendError::RecordCountOverflow)
}

#[derive(Debug, Error)]
pub enum LocalBamCanonicalBackendError {
    #[error("the alignment source has no validated immutable revision")]
    MissingSourceRevision,
    #[error("source work belongs to a different alignment source revision")]
    SourceRevisionMismatch,
    #[error("source work uses a different physical-record contract")]
    RecordContractMismatch,
    #[error("quality payload capability requires sequence capability")]
    QualitiesRequireSequence,
    #[error("a BAM payload cannot require a CRAM decode-reference revision")]
    DecodeReferenceUnsupportedForBam,
    #[error("a positioned decoder returned an owner chunk without records")]
    EmptyOwnerChunk,
    #[error("the alignment decode thread stopped unexpectedly")]
    DecoderStopped,
    #[error("the alignment decoder failed with an internal error: {0}")]
    DecoderPanicked(String),
    #[error("a checkpoint was requested without an active decode stream")]
    NoActiveStream,
    #[error("the decoder cancelled despite the actor retaining active work")]
    UnexpectedDecoderCancellation,
    #[error("an alignment record count cannot be represented as u64")]
    RecordCountOverflow,
    #[error("source completion target {actual:?} did not match active target {expected:?}")]
    CompletionTargetMismatch {
        expected: GenomicInterval,
        actual: GenomicInterval,
    },
    #[error(transparent)]
    Alignment(#[from] AlignmentError),
    #[error(transparent)]
    Payload(#[from] AlignmentPayloadError),
    #[error(transparent)]
    Chunk(#[from] ChunkError),
    #[error(transparent)]
    PayloadSidecar(#[from] PayloadSidecarError),
    #[error(transparent)]
    EvidenceChunk(#[from] EvidenceChunkError),
    #[error(transparent)]
    Coverage(#[from] CoverageError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

/// A decode source that records what the production decoder was asked to do and actually
/// read, so tests can tell a skipped block from a decoded one.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use rgv_core::GenomicInterval;
    use rgv_hts::{
        AlignmentError, PositionedAlignmentStreamCheckpoint, PositionedChunkReceipt,
        ProductionAlignmentSource, QueryOptions,
    };

    use super::PositionedDecodeSource;

    /// One source unit the decoder read.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct DecodedBlock {
        pub(crate) compressed_offset: u64,
        pub(crate) records: usize,
        /// Smallest start and largest end of the retained records.
        pub(crate) span: Option<(u64, u64)>,
    }

    /// One stream the decoder began.
    #[derive(Clone, Debug)]
    pub(crate) struct DecodedStream {
        pub(crate) target: GenomicInterval,
        pub(crate) receipts_offered: usize,
        pub(crate) receipts_honoured: usize,
        pub(crate) blocks: Vec<DecodedBlock>,
    }

    impl DecodedStream {
        pub(crate) fn offsets(&self) -> std::collections::BTreeSet<u64> {
            self.blocks
                .iter()
                .map(|block| block.compressed_offset)
                .collect()
        }

        pub(crate) fn records(&self) -> usize {
            self.blocks.iter().map(|block| block.records).sum()
        }
    }

    #[derive(Clone, Default)]
    pub(crate) struct DecodeLog(Arc<Mutex<Vec<DecodedStream>>>);

    impl DecodeLog {
        pub(crate) fn streams(&self) -> Vec<DecodedStream> {
            self.0.lock().unwrap().clone()
        }

        pub(crate) fn wrap(&self, inner: ProductionAlignmentSource) -> RecordingDecodeSource {
            RecordingDecodeSource {
                inner,
                log: self.clone(),
            }
        }
    }

    pub(crate) struct RecordingDecodeSource {
        inner: ProductionAlignmentSource,
        log: DecodeLog,
    }

    impl PositionedDecodeSource for RecordingDecodeSource {
        fn prepare_stream(&mut self, still_wanted: Arc<dyn Fn() -> bool + Send + Sync>) {
            self.inner.prepare_stream(still_wanted);
        }

        fn begin_stream(
            &mut self,
            target: GenomicInterval,
            admission_target: GenomicInterval,
            options: QueryOptions,
            skip: Vec<PositionedChunkReceipt>,
        ) -> Result<usize, AlignmentError> {
            let receipts_offered = skip.len();
            let receipts_honoured =
                self.inner
                    .begin_stream(target, admission_target, options, skip)?;
            self.log.0.lock().unwrap().push(DecodedStream {
                target,
                receipts_offered,
                receipts_honoured,
                blocks: Vec::new(),
            });
            Ok(receipts_honoured)
        }

        fn next_checkpoint(
            &mut self,
            still_wanted: &dyn Fn() -> bool,
        ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
            let checkpoint = self.inner.next_checkpoint(still_wanted)?;
            if let PositionedAlignmentStreamCheckpoint::Block(block) = &checkpoint {
                let span = block.records.iter().fold(None, |span, positioned| {
                    let (start, end) = span.unwrap_or((u64::MAX, 0));
                    Some((
                        start.min(positioned.record.start),
                        end.max(positioned.record.end),
                    ))
                });
                self.log
                    .0
                    .lock()
                    .unwrap()
                    .last_mut()
                    .expect("a block belongs to a begun stream")
                    .blocks
                    .push(DecodedBlock {
                        compressed_offset: block.compressed_offset,
                        records: block.records.len(),
                        span,
                    });
            }
            Ok(checkpoint)
        }

        fn take_chunk_receipt(&mut self) -> Option<PositionedChunkReceipt> {
            self.inner.take_chunk_receipt()
        }

        fn cancel_stream(&mut self) {
            self.inner.cancel_stream();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU64, NonZeroUsize},
        path::Path,
        sync::Arc,
    };

    use rgv_alignment_scene::{
        AlleleSummaryDetail, AllocationLedger, CapabilityRequirement, EvidenceStore,
        FilteredRecordProvenance, PayloadContract, ReservationKind, SummaryContract,
    };
    use rgv_core::{ContigId, OverlappingMatePolicy};
    use rgv_hts::{AlignmentOpenOptions, LocalAlignmentSource, QueryDetail, SequenceView};

    use super::*;
    use crate::{AlignmentSourceActor, AlignmentSourceActorEvent};

    /// A decoder whose first stream panics and whose later streams fail cleanly.
    struct ScriptedDecodeSource {
        streams: usize,
    }

    impl PositionedDecodeSource for ScriptedDecodeSource {
        fn prepare_stream(&mut self, _still_wanted: Arc<dyn Fn() -> bool + Send + Sync>) {}

        fn begin_stream(
            &mut self,
            _target: GenomicInterval,
            _admission_target: GenomicInterval,
            _options: QueryOptions,
            _skip: Vec<PositionedChunkReceipt>,
        ) -> Result<usize, AlignmentError> {
            self.streams += 1;
            assert!(self.streams > 1, "scripted decoder failure in stream one");
            Err(AlignmentError::SourceChanged)
        }

        fn next_checkpoint(
            &mut self,
            _still_wanted: &dyn Fn() -> bool,
        ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
            unreachable!("no scripted stream begins successfully")
        }

        fn cancel_stream(&mut self) {}
    }

    #[test]
    fn a_decoder_panic_fails_only_its_stream_and_the_next_begin_is_served() {
        let mut decoder = DecoderThread::spawn(ScriptedDecodeSource { streams: 0 });
        let wake_count = Arc::new(AtomicU64::new(0));
        let waker: MailboxWaker = {
            let wake_count = Arc::clone(&wake_count);
            Arc::new(move || {
                wake_count.fetch_add(1, Ordering::AcqRel);
            })
        };
        assert!(decoder.unit_ready.set(waker).is_ok());
        let target = GenomicInterval::new(rgv_core::ContigId::new(0), 10, 20).unwrap();

        decoder
            .begin(target, target, QueryOptions::default())
            .unwrap();
        match decoder.next() {
            Err(LocalBamCanonicalBackendError::DecoderPanicked(message)) => {
                assert!(message.contains("scripted decoder failure in stream one"));
            }
            other => panic!("expected the panic as a stream error, got {other:?}"),
        }

        // The decode thread survived: a new stream is accepted and answered.
        decoder
            .begin(target, target, QueryOptions::default())
            .unwrap();
        assert!(matches!(
            decoder.next(),
            Err(LocalBamCanonicalBackendError::Alignment(
                AlignmentError::SourceChanged
            ))
        ));

        // Every hand-over wakes the driver, so it never has to poll for one.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while wake_count.load(Ordering::Acquire) < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the decoder did not wake its driver"
            );
            thread::yield_now();
        }
    }

    fn no_payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            std::iter::empty(),
            None,
        )
    }

    fn fixture_backend(
        complete_read_limit: usize,
    ) -> (PositionedAlignmentBackend, GenomicInterval) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let backend = PositionedAlignmentBackend::new(
            source,
            QueryOptions {
                max_records: NonZeroUsize::new(complete_read_limit).unwrap(),
                ..QueryOptions::default()
            },
        )
        .unwrap();
        (
            backend,
            GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap(),
        )
    }

    #[test]
    fn production_local_bam_backend_admits_exact_complete_evidence() {
        let (backend, target) = fixture_backend(100);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(target, no_payload());
        let ledger = AllocationLedger::new(4 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let sequence = actor.submit_foreground(intent).unwrap();
        let mut completion = None;
        for _ in 0..32 {
            if let Some(event) = actor.pump_once().unwrap()
                && let AlignmentSourceActorEvent::AcquisitionCompleted {
                    publish,
                    admitted_revision,
                    ..
                } = event
            {
                completion = Some((publish, admitted_revision));
                break;
            }
        }

        assert_eq!(completion.unwrap().0, Some(sequence));
        assert_eq!(actor.store().acquisition_count(), 1);
        assert!(actor.store().chunk_count() >= 1);
        let candidate = actor
            .store_mut()
            .prepare(target, no_payload(), ReservationKind::Normal)
            .unwrap();
        assert!(candidate.root().coverage().covers(target));
        assert_eq!(candidate.root().record_count_upper_bound(), 13);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.reserved, 0);
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::DecoderScratch
                && usage.current == 0
                && usage.high_water > 0
        }));
    }

    #[test]
    fn production_backend_admits_exact_local_cram_evidence() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/manual-suite/data");
        let cram = root.join("alignments/reference-dependent.cram");
        let reference = root.join("references/manual.fa");
        if !cram.exists() || !reference.exists() {
            return;
        }
        let open_options = AlignmentOpenOptions {
            reference_path: Some(reference),
            ..AlignmentOpenOptions::default()
        };
        let mut oracle = rgv_hts::ProductionAlignmentSource::open(&cram, &open_options).unwrap();
        let (contig, expected) = (0..oracle.reference_dictionary().len())
            .find_map(|index| {
                let contig = ContigId::new(u32::try_from(index).unwrap());
                let length = oracle.reference_dictionary().get(contig)?.length();
                let target = GenomicInterval::new(contig, 0, length).ok()?;
                let query = oracle
                    .query(target, QueryOptions::default(), &|| true)
                    .unwrap();
                (!query.batch.is_empty()).then_some((target, query.batch.len()))
            })
            .expect("the CRAM fixture contains mapped records");

        let source = rgv_hts::ProductionAlignmentSource::open(&cram, &open_options).unwrap();
        let backend = PositionedAlignmentBackend::new(source, QueryOptions::default()).unwrap();
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(contig, no_payload());
        let ledger = AllocationLedger::new(16 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let sequence = actor.submit_foreground(intent).unwrap();
        let mut published = None;
        let mut cram_chunks = 0;
        for _ in 0..256 {
            match actor.pump_once().unwrap() {
                Some(AlignmentSourceActorEvent::ChunkAdmitted { id, .. }) => {
                    assert!(matches!(id, SourceChunkId::CramSlice { .. }));
                    cram_chunks += 1;
                }
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { publish, .. }) => {
                    published = Some(publish);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(published, Some(Some(sequence)));
        assert!(cram_chunks >= 1);
        let candidate = actor
            .store_mut()
            .prepare(contig, no_payload(), ReservationKind::Normal)
            .unwrap();
        assert!(candidate.root().coverage().covers(contig));
        assert_eq!(
            candidate.root().select(contig).unwrap().records().len(),
            expected
        );
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
    }

    #[test]
    fn nonblocking_backend_reports_pending_and_still_delivers_the_complete_transaction() {
        let (backend, target) = fixture_backend(50_000);
        let backend = backend.with_nonblocking_checkpoints();
        let ledger = AllocationLedger::new(256 << 20, 64 << 20);
        let store = EvidenceStore::new(
            backend.source_revision(),
            backend.record_contract(),
            ledger.clone(),
        );
        let spec = backend.intent_spec(target, no_payload());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(8).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(spec).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut completed = false;
        while std::time::Instant::now() < deadline {
            match actor.pump_once().unwrap() {
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. }) => {
                    completed = true;
                    break;
                }
                Some(AlignmentSourceActorEvent::SourceWaiting { .. }) => {
                    std::thread::yield_now();
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(
            completed,
            "the decode thread never finished the transaction"
        );
        assert_eq!(actor.store().acquisition_count(), 1);
        assert!(actor.store().chunk_count() > 0);

        // Cancelling and restarting on the same backend starts a clean stream.
        let spec = actor.backend().intent_spec(target, no_payload());
        actor.submit_foreground(spec).unwrap();
        let mut restarted = false;
        while std::time::Instant::now() < deadline {
            match actor.pump_once().unwrap() {
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. }) => {
                    restarted = true;
                    break;
                }
                Some(AlignmentSourceActorEvent::SourceWaiting { .. }) => {
                    std::thread::yield_now();
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(restarted, "a second stream did not complete");
    }

    #[test]
    fn production_decoder_scratch_refusal_publishes_no_partial_chunk() {
        let (backend, target) = fixture_backend(100);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(target, no_payload());
        let ledger = AllocationLedger::new_with_transient_budget(4 << 20, 0, 1);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(intent).unwrap();
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(_))
        ));
        let error = actor.pump_once().unwrap_err();
        assert!(matches!(
            error,
            crate::AlignmentSourceActorError::Work {
                source: crate::AlignmentSourceWorkError::Backend(
                    LocalBamCanonicalBackendError::Ledger(LedgerError::TransientBudgetExceeded {
                        class: TransientResourceClass::DecoderScratch,
                        ..
                    })
                ),
                ..
            }
        ));
        assert_eq!(actor.store().chunk_count(), 0);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert_eq!(snapshot.reserved, 0);
    }

    #[test]
    fn production_scene_population_and_summaries_match_a_fresh_indexed_query() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let target = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(100).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            ..QueryOptions::default()
        };

        let mut oracle_source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let contig_length = oracle_source
            .reference_dictionary()
            .get(target.contig())
            .unwrap()
            .length();
        let oracle = oracle_source
            .query(target, options.clone(), &|| true)
            .unwrap();
        assert!(oracle.individual_reads_complete());

        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let backend = PositionedAlignmentBackend::new(source, options.clone()).unwrap();
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let payload = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let intent = backend.intent_spec(target, payload.clone());
        let ledger = AllocationLedger::new(64 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(intent).unwrap();
        for _ in 0..64 {
            if matches!(
                actor.pump_once().unwrap(),
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            ) {
                break;
            }
        }
        let candidate = actor
            .store_mut()
            .prepare(target, payload, ReservationKind::Normal)
            .unwrap();
        let evidence = Arc::clone(candidate.root());
        drop(candidate);
        let selection = evidence.select(target).unwrap();
        let mut actual_records = (0..selection.len())
            .map(|index| {
                (
                    selection.records()[index].handle.source_position(),
                    selection.materialize_retained_record(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let mut expected_records = (0..oracle.batch.len())
            .map(|index| {
                (
                    oracle
                        .batch
                        .source_position(index)
                        .expect("indexed BAM oracle records carry physical positions"),
                    oracle.batch.materialize_record(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        // The evidence selection intentionally uses genomic presentation order, while the
        // independent indexed query retains physical BAM order. Compare the exact physical
        // population and full payload by their common source-position authority instead of
        // accidentally making either ordering policy part of the evidence contract.
        actual_records.sort_unstable_by_key(|(position, _)| *position);
        expected_records.sort_unstable_by_key(|(position, _)| *position);
        assert_eq!(actual_records, expected_records);
        assert_eq!(
            selection
                .records()
                .iter()
                .map(|record| record.handle)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            expected_records.len()
        );
        drop(selection);

        let summary_contract = SummaryContract {
            overlapping_mate_policy: options.overlapping_mate_policy,
            minimum_base_quality: options.minimum_base_quality,
            minimum_junction_flank: options.minimum_junction_flank,
            allele_detail: AlleleSummaryDetail::QualityWeighted,
            collect_modifications: false,
        };
        let summaries = actor
            .store_mut()
            .prepare_exact_summaries(
                &evidence,
                target,
                NonZeroU64::new(1).unwrap(),
                contig_length,
                summary_contract,
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let root = summaries.root();
        let projected_depth = root.project_depth_bins(target, 2).unwrap();
        assert_eq!(projected_depth.len(), oracle.pileup.bins.len());
        for (actual, expected) in projected_depth.iter().zip(&oracle.pileup.bins) {
            assert_eq!(actual.depth, expected.depth);
            assert_eq!(actual.forward_depth, expected.forward_depth);
            assert_eq!(actual.reverse_depth, expected.reverse_depth);
            assert_eq!(actual.mapping_quality_sum, expected.mapping_quality_sum);
            assert_eq!(
                actual.mapping_quality_observations,
                expected.mapping_quality_observations
            );
        }
        for position in target.start()..target.end() {
            let actual = root.allele_site(position).unwrap().unwrap();
            let expected = oracle.pileup.bin_at(target, position).unwrap().1;
            assert_eq!(actual.alleles, expected.alleles, "position {position}");
            assert_eq!(actual.forward_alleles, expected.forward_alleles);
            assert_eq!(actual.reverse_alleles, expected.reverse_alleles);
            assert_eq!(actual.quality_weighted, expected.quality_weighted);
            assert_eq!(actual.base_quality_sum, expected.base_quality_sum);
            assert_eq!(
                actual.base_quality_observations,
                expected.base_quality_observations
            );
        }
        assert_eq!(root.junctions(target).unwrap(), oracle.junctions.junctions);
        assert_eq!(root.pair_summary(target).unwrap(), oracle.pairs);
        assert_eq!(
            root.filter_provenance(target).unwrap(),
            FilteredRecordProvenance {
                records_seen: oracle.stats.records_seen as u64,
                eligible_records: oracle.stats.records_eligible as u64,
                unmapped: oracle.stats.records_filtered_unmapped as u64,
                secondary: oracle.stats.records_filtered_secondary as u64,
                supplementary: oracle.stats.records_filtered_supplementary as u64,
                user_filter: oracle.stats.records_filtered_user as u64,
            }
        );
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
    }

    #[test]
    fn production_local_bam_backend_refuses_dense_reads_without_false_acquisition() {
        let (backend, target) = fixture_backend(1);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(target, no_payload());
        let ledger = AllocationLedger::new(4 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let sequence = actor.submit_foreground(intent).unwrap();
        let mut refusal = None;
        for _ in 0..32 {
            if let Some(event) = actor.pump_once().unwrap()
                && let AlignmentSourceActorEvent::AcquisitionRefused {
                    coverage,
                    complete_read_limit,
                    publish,
                    ..
                } = event
            {
                refusal = Some((coverage, complete_read_limit, publish));
                break;
            }
        }

        let (coverage, limit, publish) = refusal.expect("dense fixture should be refused");
        assert_eq!(coverage.provenance().eligible_records, 13);
        assert_eq!(limit, 1);
        assert_eq!(publish, Some(sequence));
        assert_eq!(actor.store().acquisition_count(), 0);
        assert!(actor.store().chunk_count() <= 1);
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
    }

    #[test]
    fn production_local_bam_backend_keeps_requested_payload_out_of_core_identity() {
        let (backend, target) = fixture_backend(100);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"RG"],
            None,
        );
        let intent = backend.intent_spec(target, payload.clone());
        let ledger = AllocationLedger::new(4 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(intent).unwrap();
        for _ in 0..32 {
            if matches!(
                actor.pump_once().unwrap(),
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            ) {
                break;
            }
        }

        let candidate = actor
            .store_mut()
            .prepare(target, payload, ReservationKind::Normal)
            .unwrap();
        let selected = candidate.root().select(target).unwrap();
        assert_eq!(selected.len(), 13);
        assert!((0..selected.len()).all(|index| matches!(
            selected.record_view(index).unwrap().sequence(),
            SequenceView::NotLoaded
        )));
        assert!((0..selected.len()).all(|index| matches!(
            selected.sequence(index),
            Some(SequenceView::Present {
                qualities: Some(_),
                ..
            })
        )));
        assert!(ledger.snapshot().unwrap().records.iter().any(|allocation| {
            allocation.class == AllocationClass::PayloadSidecar && allocation.charged_capacity > 0
        }));
    }

    // --- Chunk-level resume -------------------------------------------------------------

    use super::test_support::{DecodeLog, DecodedStream};

    type ResumeActor = AlignmentSourceActor<PositionedAlignmentBackend>;

    fn sequence_payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            std::iter::empty(),
            None,
        )
    }

    /// An actor over the multi-megabyte benchmark BAM whose decoder reads are logged.
    fn mixed_actor() -> (ResumeActor, DecodeLog) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let source =
            rgv_hts::ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default())
                .unwrap();
        let log = DecodeLog::default();
        let backend = PositionedAlignmentBackend::with_decode_source(
            source,
            QueryOptions {
                max_records: NonZeroUsize::new(1_000_000).unwrap(),
                ..QueryOptions::default()
            },
            |source| log.wrap(source),
        )
        .unwrap();
        let store = EvidenceStore::new(
            backend.source_revision(),
            backend.record_contract(),
            AllocationLedger::new(2 << 30, 0),
        );
        let actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        (actor, log)
    }

    fn mixed_interval(start: u64, end: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(1), start, end).unwrap()
    }

    /// Pumps the actor until its active transaction is admitted, returning the chunk ids
    /// admitted on the way.
    fn pump_to_completion(actor: &mut ResumeActor) -> Vec<SourceChunkId> {
        let mut admitted = Vec::new();
        for _ in 0..100_000 {
            match actor.pump_once().unwrap() {
                Some(AlignmentSourceActorEvent::ChunkAdmitted { id, .. }) => admitted.push(id),
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. }) => return admitted,
                Some(AlignmentSourceActorEvent::AcquisitionRefused { .. }) => {
                    panic!("the fixture target was refused")
                }
                Some(_) => {}
                None => panic!("the actor went idle before completing"),
            }
        }
        panic!("the transaction did not complete");
    }

    fn acquire(
        actor: &mut ResumeActor,
        target: GenomicInterval,
        payload: &PayloadContract,
    ) -> Vec<SourceChunkId> {
        let spec = actor.backend().intent_spec(target, payload.clone());
        actor.submit_foreground(spec).unwrap();
        pump_to_completion(actor)
    }

    /// Every record the store's evidence holds for `target`, by physical position.
    fn population(
        actor: &mut ResumeActor,
        target: GenomicInterval,
        payload: &PayloadContract,
    ) -> Vec<(rgv_hts::AlignmentSourcePosition, rgv_hts::AlignmentRecord)> {
        let candidate = actor
            .store_mut()
            .prepare(target, payload.clone(), ReservationKind::Normal)
            .unwrap();
        assert!(candidate.root().coverage().covers(target));
        let selection = candidate.root().select(target).unwrap();
        let mut records = (0..selection.len())
            .map(|index| {
                (
                    selection.records()[index].handle.source_position(),
                    selection.materialize_retained_record(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        records.sort_unstable_by_key(|(position, _)| *position);
        records
    }

    /// What one fresh request for `target` decodes and holds.
    fn fresh(
        target: GenomicInterval,
        payload: &PayloadContract,
    ) -> (
        DecodedStream,
        Vec<(rgv_hts::AlignmentSourcePosition, rgv_hts::AlignmentRecord)>,
    ) {
        let (mut actor, log) = mixed_actor();
        acquire(&mut actor, target, payload);
        let streams = log.streams();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].receipts_offered, 0);
        let records = population(&mut actor, target, payload);
        (streams.into_iter().next().unwrap(), records)
    }

    const HELD: (u64, u64) = (400_000, 1_200_000);
    const PANNED: (u64, u64) = (800_000, 1_600_000);

    #[test]
    fn a_pan_beyond_the_acquired_window_decodes_only_the_blocks_it_does_not_hold() {
        let payload = no_payload();
        let held = mixed_interval(HELD.0, HELD.1);
        let panned = mixed_interval(PANNED.0, PANNED.1);
        let (mut actor, log) = mixed_actor();
        acquire(&mut actor, held, &payload);
        let first = log.streams().pop().unwrap();
        assert!(
            first.blocks.len() > 20,
            "the fixture window must span many BGZF blocks, not {}",
            first.blocks.len()
        );
        let held_receipts = actor.backend().decode_telemetry();
        assert_eq!(held_receipts.receipts_held, first.blocks.len());
        assert!(held_receipts.receipt_bytes > 0);
        assert!(held_receipts.receipt_bytes <= RECEIPT_BYTE_LIMIT);

        acquire(&mut actor, panned, &payload);
        let streams = log.streams();
        assert_eq!(streams.len(), 2);
        let resumed = &streams[1];
        assert_eq!((streams[0].target, resumed.target), (held, panned));
        let (fresh_stream, fresh_records) = fresh(panned, &payload);

        // The resumed stream read a strict subset of what a fresh one reads, and what it left
        // out is exactly the blocks whose receipts it honoured, all of them held.
        let resumed_offsets = resumed.offsets();
        let fresh_offsets = fresh_stream.offsets();
        let held_offsets = first.offsets();
        assert!(resumed_offsets.is_subset(&fresh_offsets));
        let skipped = fresh_offsets
            .difference(&resumed_offsets)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(skipped.is_subset(&held_offsets));
        assert_eq!(resumed.receipts_offered, resumed.receipts_honoured);
        assert_eq!(skipped.len(), resumed.receipts_honoured);
        // Of the held blocks the new target needs, at most the one at the old right edge is
        // read again: the old stream could not prove it held no record beyond its target.
        let shared = fresh_offsets
            .intersection(&held_offsets)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(shared.len() > 10, "the pan must overlap many held blocks");
        let reread = shared.intersection(&resumed_offsets).count();
        assert!(reread <= 1, "{reread} held blocks were decoded again");
        assert!(resumed.records() < fresh_stream.records());
        let telemetry = actor.backend().decode_telemetry();
        assert_eq!(telemetry.streams_started, 2);
        assert_eq!(telemetry.receipts_honoured, skipped.len() as u64);
        assert_eq!(
            telemetry.blocks_decoded,
            (first.blocks.len() + resumed.blocks.len()) as u64
        );

        // And the evidence for the panned target is what one fresh request for it holds.
        assert_eq!(actor.store().acquisition_count(), 2);
        assert_eq!(population(&mut actor, panned, &payload), fresh_records);
    }

    fn positions(
        records: &[(rgv_hts::AlignmentSourcePosition, rgv_hts::AlignmentRecord)],
    ) -> Vec<rgv_hts::AlignmentSourcePosition> {
        records.iter().map(|(position, _)| *position).collect()
    }

    /// The EPI2ME `wf-human-variation` demo: HG002 nanopore reads up to ~31 kb with CIGARs of
    /// ~1,000 operations and MM/ML tags. It is not in the repository.
    const ONT_DEMO_BAM: &str = "/Volumes/Extreme SSD/rgv_test_data/long-read-alignments/\
                                wf-human-variation-demo-download/wf-human-variation-demo/demo.bam";

    /// An actor over the ONT demo BAM as the desktop opens it, and the id of `chr20`.
    fn ont_demo_actor() -> (ResumeActor, ContigId) {
        let source = rgv_hts::ProductionAlignmentSource::open(
            Path::new(ONT_DEMO_BAM),
            &AlignmentOpenOptions::default(),
        )
        .unwrap();
        let chr20 = source.reference_dictionary().id_by_name("chr20").unwrap();
        let backend = PositionedAlignmentBackend::new(source, QueryOptions::default()).unwrap();
        let store = EvidenceStore::new(
            backend.source_revision(),
            backend.record_contract(),
            AllocationLedger::new(2 << 30, 0),
        );
        let actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        (actor, chr20)
    }

    /// The reported failure: browsing around chr20:187,097-225,104 of the ONT demo failed with
    /// "record N repeats an existing physical source position". The desktop fetches the
    /// window, then (speculatively) the uncached windows beside it; a later pan across held
    /// chunks resumed from them and decoded a read twice. Every window must load, hold each
    /// read once, and hold what a fresh load holds.
    #[test]
    #[ignore = "needs the EPI2ME ONT demo BAM on the external test-data drive"]
    fn browsing_the_ont_demo_bam_loads_every_window_with_each_read_once() {
        let payload = sequence_payload();
        let (mut actor, chr20) = ont_demo_actor();
        for (start, end) in [
            (187_097, 225_104),
            (225_104, 263_111),
            (149_090, 187_097),
            (263_111, 301_118),
            (111_083, 149_090),
            // This pan failed before the fix.
            (230_473, 306_055),
            (200_000, 240_000),
            (150_000, 260_000),
            (187_097, 225_104),
        ] {
            let target = GenomicInterval::new(chr20, start, end).unwrap();
            acquire(&mut actor, target, &payload);
            let held = positions(&population(&mut actor, target, &payload));
            let unique = held.iter().collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                unique.len(),
                held.len(),
                "{start}..{end}: a read is held twice"
            );
            let (mut fresh_actor, _) = ont_demo_actor();
            acquire(&mut fresh_actor, target, &payload);
            assert_eq!(
                held,
                positions(&population(&mut fresh_actor, target, &payload)),
                "{start}..{end}: the resumed window differs from a fresh load"
            );
        }
    }

    #[test]
    fn a_receipt_for_an_evicted_chunk_is_not_used_and_the_result_is_still_complete() {
        let payload = no_payload();
        let held = mixed_interval(HELD.0, HELD.1);
        let panned = mixed_interval(PANNED.0, PANNED.1);
        let (fresh_stream, fresh_records) = fresh(panned, &payload);
        let fresh_offsets = fresh_stream.offsets();

        // Everything evicted: no receipt is offered, every block is decoded, and the receipts
        // of the chunks that are gone are let go of.
        let (mut actor, log) = mixed_actor();
        acquire(&mut actor, held, &payload);
        let elsewhere = GenomicInterval::new(ContigId::new(0), 0, 1).unwrap();
        assert!(actor.store_mut().discard_disjoint_cache(elsewhere) > 0);
        assert_eq!(actor.store().chunk_count(), 0);
        acquire(&mut actor, panned, &payload);
        let resumed = log.streams().pop().unwrap();
        assert_eq!(resumed.receipts_offered, 0);
        assert_eq!(resumed.offsets(), fresh_offsets);
        assert_eq!(resumed.records(), fresh_stream.records());
        assert_eq!(
            actor.backend().decode_telemetry().receipts_held,
            resumed.blocks.len()
        );
        assert_eq!(population(&mut actor, panned, &payload), fresh_records);

        // Part of the window evicted: the store keeps only chunks reaching into `kept`. The
        // blocks of the others are decoded again; the blocks still resident are skipped.
        let (mut actor, log) = mixed_actor();
        acquire(&mut actor, held, &payload);
        let first = log.streams().pop().unwrap();
        let kept = mixed_interval(HELD.0, 1_000_000);
        let chunks_before = actor.store().chunk_count();
        actor.store_mut().discard_disjoint_cache(kept);
        let evicted = first
            .blocks
            .iter()
            .filter(|block| {
                block
                    .span
                    .is_some_and(|(start, end)| start >= kept.end() || end <= kept.start())
            })
            .map(|block| block.compressed_offset)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(evicted.len() > 10, "the eviction must remove many chunks");
        assert_eq!(
            actor.store().chunk_count(),
            chunks_before - evicted.len(),
            "exactly the chunks outside the kept interval were evicted"
        );
        acquire(&mut actor, panned, &payload);
        let resumed = log.streams().pop().unwrap();
        let resumed_offsets = resumed.offsets();
        let needed_evicted = evicted
            .intersection(&fresh_offsets)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(needed_evicted.len() > 10);
        assert!(
            needed_evicted.is_subset(&resumed_offsets),
            "a block whose chunk was evicted was skipped"
        );
        assert!(
            resumed.receipts_honoured > 10,
            "resident chunks were skipped"
        );
        assert_eq!(
            resumed.blocks.len() + resumed.receipts_honoured,
            fresh_stream.blocks.len()
        );
        assert_eq!(population(&mut actor, panned, &payload), fresh_records);
    }

    #[test]
    fn a_retarget_to_a_payload_the_resident_chunks_cannot_serve_ignores_their_receipts() {
        let held = mixed_interval(HELD.0, HELD.1);
        let panned = mixed_interval(PANNED.0, PANNED.1);

        // Chunks decoded without sequence cannot stand in for chunks that need it.
        let (mut actor, log) = mixed_actor();
        acquire(&mut actor, held, &no_payload());
        acquire(&mut actor, panned, &sequence_payload());
        let resumed = log.streams().pop().unwrap();
        let (fresh_stream, fresh_records) = fresh(panned, &sequence_payload());
        assert_eq!(resumed.receipts_offered, 0);
        assert_eq!(resumed.offsets(), fresh_stream.offsets());
        assert_eq!(resumed.records(), fresh_stream.records());
        let records = population(&mut actor, panned, &sequence_payload());
        assert!(records.iter().all(|(_, record)| {
            matches!(record.sequence, rgv_hts::SequenceData::Present { .. })
        }));
        assert_eq!(records, fresh_records);

        // The other way round the resident chunks hold more than is asked for, and are used.
        let (mut actor, log) = mixed_actor();
        acquire(&mut actor, held, &sequence_payload());
        acquire(&mut actor, panned, &no_payload());
        let resumed = log.streams().pop().unwrap();
        let (fresh_stream, fresh_records) = fresh(panned, &no_payload());
        assert!(resumed.receipts_honoured > 10);
        assert_eq!(
            resumed.blocks.len() + resumed.receipts_honoured,
            fresh_stream.blocks.len()
        );
        assert_eq!(
            positions(&population(&mut actor, panned, &no_payload())),
            positions(&fresh_records)
        );
    }

    /// Starts a stream for the held window and returns once `chunks` of its chunks are
    /// admitted, with their block offsets.
    fn start_held_stream(actor: &mut ResumeActor, chunks: usize) -> Vec<u64> {
        let spec = actor
            .backend()
            .intent_spec(mixed_interval(HELD.0, HELD.1), no_payload());
        actor.submit_foreground(spec).unwrap();
        let mut delivered = Vec::new();
        while delivered.len() < chunks {
            match actor.pump_once().unwrap() {
                Some(AlignmentSourceActorEvent::ChunkAdmitted {
                    id: SourceChunkId::BamBlock { compressed_offset },
                    ..
                }) => delivered.push(compressed_offset),
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. }) | None => {
                    panic!("the held window finished before {chunks} chunks")
                }
                Some(_) => {}
            }
        }
        delivered
    }

    /// The delivered blocks that hold a record of `target`, from a complete log of the window.
    fn delivered_in(
        delivered: &[u64],
        window: &DecodedStream,
        target: GenomicInterval,
    ) -> std::collections::BTreeSet<u64> {
        window
            .blocks
            .iter()
            .filter(|block| delivered.contains(&block.compressed_offset))
            .filter(|block| {
                block
                    .span
                    .is_some_and(|(start, end)| start < target.end() && end > target.start())
            })
            .map(|block| block.compressed_offset)
            .collect()
    }

    #[test]
    fn a_retarget_of_the_running_stream_does_not_decode_its_delivered_chunks_again() {
        let payload = no_payload();
        let (window, _) = fresh(mixed_interval(HELD.0, HELD.1), &payload);

        // Overlapping the chunk being decoded: the running work is retargeted in place.
        let (mut actor, log) = mixed_actor();
        let delivered = start_held_stream(&mut actor, 100);
        let target = mixed_interval(HELD.0 + 1_000, PANNED.1);
        let spec = actor.backend().intent_spec(target, payload.clone());
        actor.submit_foreground(spec).unwrap();
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkRetargeted { .. })
        ));
        pump_to_completion(&mut actor);
        let resumed = log.streams().pop().unwrap();
        let expected_skipped = delivered_in(&delivered, &window, target);
        assert!(expected_skipped.len() > 50);
        assert!(
            expected_skipped.is_disjoint(&resumed.offsets()),
            "a chunk the superseded stream delivered was decoded again"
        );
        assert!(resumed.receipts_honoured >= expected_skipped.len());
        let (fresh_stream, fresh_records) = fresh(target, &payload);
        assert!(resumed.blocks.len() + expected_skipped.len() <= fresh_stream.blocks.len());
        assert_eq!(population(&mut actor, target, &payload), fresh_records);

        // Not overlapping the chunk being decoded: the work yields and a new one starts. It
        // lies inside what was delivered, so nothing it needs is decoded again.
        let (mut actor, log) = mixed_actor();
        let delivered = start_held_stream(&mut actor, 100);
        let inner = window
            .blocks
            .iter()
            .filter(|block| delivered[10..40].contains(&block.compressed_offset))
            .filter_map(|block| block.span)
            .fold((u64::MAX, 0), |(start, end), span| {
                (start.min(span.0), end.max(span.1))
            });
        // Clear of the first delivered blocks' reach, so no record of the target lies in a
        // block the yielded stream had not reached.
        let target = mixed_interval(inner.0, inner.0 + (inner.1 - inner.0) / 4);
        let spec = actor.backend().intent_spec(target, payload.clone());
        actor.submit_foreground(spec).unwrap();
        let mut yielded = false;
        let mut started = false;
        for _ in 0..8 {
            match actor.pump_once().unwrap() {
                Some(AlignmentSourceActorEvent::WorkYielded { .. }) => yielded = true,
                Some(AlignmentSourceActorEvent::WorkStarted(_)) => {
                    started = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(yielded && started, "the disjoint target replaces the work");
        pump_to_completion(&mut actor);
        let resumed = log.streams().pop().unwrap();
        let expected_skipped = delivered_in(&delivered, &window, target);
        assert!(!expected_skipped.is_empty());
        assert!(expected_skipped.is_disjoint(&resumed.offsets()));
        assert!(resumed.receipts_honoured >= expected_skipped.len());
        let (_, fresh_records) = fresh(target, &payload);
        assert!(!fresh_records.is_empty());
        assert_eq!(population(&mut actor, target, &payload), fresh_records);
    }

    #[test]
    fn held_receipts_stay_within_their_byte_limit_and_the_oldest_go_first() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let mut source =
            rgv_hts::ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default())
                .unwrap();
        let target = mixed_interval(HELD.0, HELD.1);
        source
            .begin_positioned_stream_resuming(target, target, QueryOptions::default(), Vec::new())
            .unwrap();
        let mut receipts = Vec::new();
        while let PositionedAlignmentStreamCheckpoint::Block(_) =
            source.next_positioned_stream_checkpoint(&|| true).unwrap()
        {
            receipts.extend(source.take_positioned_chunk_receipt());
        }
        assert!(receipts.len() > 40);
        let limit = receipts[..8]
            .iter()
            .map(PositionedChunkReceipt::estimated_bytes)
            .sum::<usize>();

        let mut book = ReceiptBook::new(limit);
        for receipt in &receipts {
            book.hold(receipt.clone(), None, (0, 0));
            assert!(book.bytes <= limit, "{} > {limit}", book.bytes);
            assert_eq!(
                book.bytes,
                book.held.values().map(|held| held.bytes).sum::<usize>()
            );
            // The receipt just held is never the one dropped.
            assert!(book.held.contains_key(&receipt_key(receipt)));
        }
        assert!(!book.held.is_empty() && book.held.len() < receipts.len());
        assert!(!book.held.contains_key(&receipt_key(&receipts[0])));
        let oldest_held = book.held.values().map(|held| held.age).min().unwrap();
        assert_eq!(
            oldest_held as usize,
            receipts.len() - book.held.len(),
            "exactly the oldest receipts were dropped"
        );

        // Holding a chunk's receipt again replaces it rather than counting it twice.
        let bytes = book.bytes;
        let last = receipts.last().unwrap();
        book.hold(last.clone(), None, (0, 0));
        assert_eq!(book.bytes, bytes);

        // The receipt of a chunk that nobody holds any more is dropped as holding goes on
        // (at every `RECEIPT_PRUNE_INTERVAL`th receipt), long before the byte limit is reached.
        let mut book = ReceiptBook::new(usize::MAX);
        book.hold(receipts[0].clone(), Some(Weak::new()), (0, 0));
        for receipt in receipts[1..]
            .iter()
            .cycle()
            .take(usize::try_from(RECEIPT_PRUNE_INTERVAL).unwrap() - 1)
        {
            assert!(book.held.contains_key(&receipt_key(&receipts[0])));
            book.hold(receipt.clone(), None, (0, 0));
        }
        assert!(!book.held.contains_key(&receipt_key(&receipts[0])));
        assert_eq!(book.held.len(), receipts.len() - 1);
        assert_eq!(
            book.bytes,
            book.held.values().map(|held| held.bytes).sum::<usize>()
        );

        // A receipt that could never fit is not held at all.
        let mut tiny = ReceiptBook::new(1);
        tiny.hold(receipts[0].clone(), None, (0, 0));
        assert!(tiny.held.is_empty());
        assert_eq!(tiny.bytes, 0);
    }

    // --- Process-wide decode allowance --------------------------------------------------

    use std::sync::{Condvar, Mutex, atomic::AtomicUsize};

    /// Where gated decoders stand: how many are inside a unit, and whether they may leave.
    #[derive(Default)]
    struct DecodeGate {
        inside: AtomicUsize,
        peak: AtomicUsize,
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl DecodeGate {
        fn pass(&self) {
            let inside = self.inside.fetch_add(1, Ordering::AcqRel) + 1;
            self.peak.fetch_max(inside, Ordering::AcqRel);
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
            drop(open);
            self.inside.fetch_sub(1, Ordering::AcqRel);
        }

        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.opened.notify_all();
        }
    }

    /// A decoder of `blocks` empty units, each of which waits at the gate, ending in an error.
    struct GatedDecodeSource {
        name: usize,
        gate: Arc<DecodeGate>,
        begun: Arc<Mutex<Vec<usize>>>,
        blocks: u64,
    }

    impl PositionedDecodeSource for GatedDecodeSource {
        fn prepare_stream(&mut self, _still_wanted: Arc<dyn Fn() -> bool + Send + Sync>) {}

        fn begin_stream(
            &mut self,
            _target: GenomicInterval,
            _admission_target: GenomicInterval,
            _options: QueryOptions,
            _skip: Vec<PositionedChunkReceipt>,
        ) -> Result<usize, AlignmentError> {
            self.begun.lock().unwrap().push(self.name);
            Ok(0)
        }

        fn next_checkpoint(
            &mut self,
            _still_wanted: &dyn Fn() -> bool,
        ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
            self.gate.pass();
            if self.blocks == 0 {
                return Err(AlignmentError::SourceChanged);
            }
            self.blocks -= 1;
            Ok(PositionedAlignmentStreamCheckpoint::Block(
                PositionedAlignmentBlock {
                    compressed_offset: self.blocks,
                    records: Vec::new(),
                    genomic_span: None,
                },
            ))
        }

        fn cancel_stream(&mut self) {}
    }

    fn wait_until(what: &str, condition: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            thread::yield_now();
        }
    }

    /// Reads a gated decoder to the end of its stream, returning how many units it delivered.
    fn drain_gated(decoder: &mut DecoderThread) -> u64 {
        let mut blocks = 0;
        loop {
            match decoder.next() {
                Ok(Some((PositionedAlignmentStreamCheckpoint::Block(_), _))) => blocks += 1,
                Err(LocalBamCanonicalBackendError::Alignment(AlignmentError::SourceChanged)) => {
                    return blocks;
                }
                other => panic!("unexpected gated decoder result: {other:?}"),
            }
        }
    }

    #[test]
    fn no_more_sources_decode_at_once_than_the_allowance_and_every_stream_is_served() {
        const SOURCES: usize = 6;
        const LIMIT: usize = 2;
        const BLOCKS: u64 = 5;
        let allowance = Arc::new(DecodeAllowance::new(NonZeroUsize::new(LIMIT).unwrap()));
        let gate = Arc::new(DecodeGate::default());
        let begun = Arc::new(Mutex::new(Vec::new()));
        let target = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        let mut decoders = (0..SOURCES)
            .map(|name| {
                DecoderThread::spawn_with_allowance(
                    GatedDecodeSource {
                        name,
                        gate: Arc::clone(&gate),
                        begun: Arc::clone(&begun),
                        blocks: BLOCKS,
                    },
                    Arc::clone(&allowance),
                )
            })
            .collect::<Vec<_>>();
        for (index, decoder) in decoders.iter_mut().enumerate() {
            let class = if index % 2 == 0 {
                DecodeClass::Foreground
            } else {
                DecodeClass::Speculative
            };
            decoder
                .begin_resuming(target, target, QueryOptions::default(), Vec::new(), class)
                .unwrap();
        }

        // Every source wants to decode. Exactly the allowance are inside a unit; the rest wait
        // for a permit and stay outside, however long the units take.
        wait_until("the allowance fills and the other sources queue", || {
            let snapshot = allowance.snapshot();
            snapshot.active == LIMIT
                && snapshot.waiting == SOURCES - LIMIT
                && gate.inside.load(Ordering::Acquire) == LIMIT
        });
        assert_eq!(gate.peak.load(Ordering::Acquire), LIMIT);

        gate.open();
        for decoder in &mut decoders {
            assert_eq!(drain_gated(decoder), BLOCKS);
        }
        assert_eq!(begun.lock().unwrap().len(), SOURCES);
        // The gate counted for itself: the bound held for every unit, not only the first.
        assert_eq!(gate.peak.load(Ordering::Acquire), LIMIT);
        let snapshot = allowance.snapshot();
        assert_eq!(snapshot.peak_active, LIMIT);
        assert_eq!((snapshot.active, snapshot.waiting), (0, 0));
        let units = SOURCES as u64 * (BLOCKS + 2);
        assert_eq!(
            snapshot.foreground_grants + snapshot.speculative_grants,
            units
        );
        assert_eq!(snapshot.foreground_grants, snapshot.speculative_grants);
    }

    #[test]
    fn a_foreground_stream_is_served_ahead_of_read_ahead_that_queued_first() {
        let allowance = Arc::new(DecodeAllowance::new(NonZeroUsize::new(1).unwrap()));
        let gate = Arc::new(DecodeGate::default());
        let begun = Arc::new(Mutex::new(Vec::new()));
        let target = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        let spawn = |name| {
            DecoderThread::spawn_with_allowance(
                GatedDecodeSource {
                    name,
                    gate: Arc::clone(&gate),
                    begun: Arc::clone(&begun),
                    blocks: 3,
                },
                Arc::clone(&allowance),
            )
        };
        let (mut holder, mut read_ahead, mut foreground) = (spawn(0), spawn(1), spawn(2));

        // One source holds the only permit inside a unit.
        holder
            .begin_resuming(
                target,
                target,
                QueryOptions::default(),
                Vec::new(),
                DecodeClass::Speculative,
            )
            .unwrap();
        wait_until("the holder is inside a unit", || {
            gate.inside.load(Ordering::Acquire) == 1
        });
        // Read-ahead queues first, a foreground stream second.
        read_ahead
            .begin_resuming(
                target,
                target,
                QueryOptions::default(),
                Vec::new(),
                DecodeClass::Speculative,
            )
            .unwrap();
        wait_until("read-ahead queues", || allowance.snapshot().waiting == 1);
        foreground
            .begin_resuming(
                target,
                target,
                QueryOptions::default(),
                Vec::new(),
                DecodeClass::Foreground,
            )
            .unwrap();
        wait_until("the foreground stream queues", || {
            allowance.snapshot().waiting == 2
        });
        assert_eq!(*begun.lock().unwrap(), vec![0]);

        gate.open();
        // The permit the holder gives back goes to the foreground stream, although the
        // read-ahead stream asked first; every stream is then served to its end.
        assert_eq!(drain_gated(&mut foreground), 3);
        assert_eq!(drain_gated(&mut holder), 3);
        assert_eq!(drain_gated(&mut read_ahead), 3);
        assert_eq!(*begun.lock().unwrap(), vec![0, 2, 1]);
        assert_eq!(allowance.snapshot().peak_active, 1);
    }

    #[test]
    fn a_stream_cancelled_while_waiting_for_its_turn_leaves_the_line() {
        let allowance = Arc::new(DecodeAllowance::new(NonZeroUsize::new(1).unwrap()));
        let gate = Arc::new(DecodeGate::default());
        let begun = Arc::new(Mutex::new(Vec::new()));
        let target = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        let spawn = |name| {
            DecoderThread::spawn_with_allowance(
                GatedDecodeSource {
                    name,
                    gate: Arc::clone(&gate),
                    begun: Arc::clone(&begun),
                    blocks: 2,
                },
                Arc::clone(&allowance),
            )
        };
        let (mut holder, mut waiting) = (spawn(0), spawn(1));
        holder
            .begin(target, target, QueryOptions::default())
            .unwrap();
        wait_until("the holder is inside a unit", || {
            gate.inside.load(Ordering::Acquire) == 1
        });
        waiting
            .begin(target, target, QueryOptions::default())
            .unwrap();
        wait_until("the second stream queues", || {
            allowance.snapshot().waiting == 1
        });

        waiting.cancel();
        wait_until("the cancelled stream leaves the line", || {
            allowance.snapshot().waiting == 0
        });
        assert_eq!(allowance.snapshot().withdrawn_waits, 1);
        assert_eq!(*begun.lock().unwrap(), vec![0]);

        // Its decode thread is free to serve the next stream once a permit is.
        waiting
            .begin(target, target, QueryOptions::default())
            .unwrap();
        gate.open();
        assert_eq!(drain_gated(&mut holder), 2);
        assert_eq!(drain_gated(&mut waiting), 2);
        assert_eq!(*begun.lock().unwrap(), vec![0, 1]);
    }
}
