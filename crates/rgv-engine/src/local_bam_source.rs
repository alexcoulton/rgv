use std::{
    mem::size_of,
    num::NonZeroUsize,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use rgv_alignment_scene::{
    AlignmentHandle, AlignmentSourceRevision, AllocationClass, AllocationLedger,
    CanonicalRecordChunk, CapabilityRequirement, ChunkError, CompleteCoverage, ContractFingerprint,
    CoverageError, EvidenceChunk, EvidenceChunkError, FilteredRecordProvenance, LedgerError,
    PayloadContract, PayloadSidecar, PayloadSidecarError, RecordContract, ReservationKind,
    SourceChunkId, TransientResourceClass,
};
use rgv_core::{GenomicInterval, OverlappingMatePolicy};
use rgv_hts::{
    AlignmentBatch, AlignmentError, AlignmentPayloadBatch, AlignmentPayloadError, AlignmentSource,
    IndividualReadStatus, PositionedAlignmentBlock, PositionedAlignmentStreamCheckpoint,
    ProductionAlignmentSource, QueryDetail, QueryOptions, QueryStats, RemoteTransferBudget,
};
use thiserror::Error;

use crate::{
    AlignmentSourceIntentSpec, CanonicalAlignmentSourceBackend, CanonicalDecodeCheckpoint,
    SourceWork,
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
/// Despite its historical name this backend serves every production alignment source: local BAM
/// through `HTSlib`, and local CRAM, remote BAM and remote CRAM through the positioned pure-Rust
/// decoders. One instance owns one mutable indexed reader. It never certifies a partial
/// transaction and it returns to the actor after every physical owner chunk (BGZF block or CRAM
/// slice), including chunks containing only filtered records.
pub struct LocalBamCanonicalBackend {
    decoder: DecoderThread,
    is_cram: bool,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    options: QueryOptions,
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
}

struct DecodeResult {
    generation: u64,
    checkpoint: Result<PositionedAlignmentStreamCheckpoint, DecodeFailure>,
}

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
trait PositionedDecodeSource: Send + 'static {
    fn prepare_stream(&mut self, still_wanted: Arc<dyn Fn() -> bool + Send + Sync>);

    fn begin_stream(
        &mut self,
        target: GenomicInterval,
        admission_target: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), AlignmentError>;

    fn next_checkpoint(
        &mut self,
        still_wanted: &dyn Fn() -> bool,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError>;

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
    ) -> Result<(), AlignmentError> {
        self.begin_positioned_stream_with_admission(target, admission_target, options)
    }

    fn next_checkpoint(
        &mut self,
        still_wanted: &dyn Fn() -> bool,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
        self.next_positioned_stream_checkpoint(still_wanted)
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
    worker: Option<JoinHandle<()>>,
}

impl DecoderThread {
    fn spawn(source: impl PositionedDecodeSource) -> Self {
        let (requests, request_rx) = mpsc::channel::<DecodeRequest>();
        let (result_tx, results) = mpsc::sync_channel::<DecodeResult>(DECODE_READ_AHEAD);
        let wanted = Arc::new(AtomicU64::new(0));
        let thread_wanted = Arc::clone(&wanted);
        let unit_ready = Arc::new(OnceLock::new());
        let thread_unit_ready = Arc::clone(&unit_ready);
        let worker = thread::Builder::new()
            .name("rgv-alignment-decode".to_owned())
            .spawn(move || {
                decode_loop(
                    source,
                    &request_rx,
                    &result_tx,
                    &thread_wanted,
                    &thread_unit_ready,
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
            worker: Some(worker),
        }
    }

    fn begin(
        &mut self,
        target: GenomicInterval,
        admission_target: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), LocalBamCanonicalBackendError> {
        self.last_generation += 1;
        let generation = self.last_generation;
        self.wanted.store(generation, Ordering::Release);
        self.active = Some(generation);
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
                    })
                    .ok()
            })
            .ok_or(LocalBamCanonicalBackendError::DecoderStopped)
    }

    fn cancel(&mut self) {
        self.active = None;
        self.wanted.store(0, Ordering::Release);
        while self.results.try_recv().is_ok() {}
    }

    /// The next finished unit of the active stream, or `None` when the decoder is still busy.
    fn next(
        &mut self,
    ) -> Result<Option<PositionedAlignmentStreamCheckpoint>, LocalBamCanonicalBackendError> {
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
                    return Ok(Some(checkpoint));
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
        let deliver = |checkpoint| {
            let delivered = results
                .send(DecodeResult {
                    generation,
                    checkpoint,
                })
                .is_ok();
            if let Some(wake) = unit_ready.get() {
                wake();
            }
            delivered
        };
        let stream = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_stream(&mut source, request, &still_wanted, &deliver)
        }));
        match stream {
            Ok(true) => {}
            Ok(false) => return,
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                tracing::error!(message, "alignment decoder panicked; the stream was abandoned");
                // Best effort: leave the reader without a half-consumed stream. If this panics
                // too, the next stream's own containment reports it.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.cancel_stream();
                }));
                if still_wanted() && !deliver(Err(DecodeFailure::Panicked(message))) {
                    return;
                }
            }
        }
    }
}

/// Decodes one stream to its terminal result. Returns `false` once the backend is gone.
fn decode_stream<S: PositionedDecodeSource>(
    source: &mut S,
    request: DecodeRequest,
    still_wanted: &(impl Fn() -> bool + Clone + Send + Sync + 'static),
    deliver: &impl Fn(Result<PositionedAlignmentStreamCheckpoint, DecodeFailure>) -> bool,
) -> bool {
    source.prepare_stream(Arc::new(still_wanted.clone()));
    if let Err(error) =
        source.begin_stream(request.target, request.admission_target, request.options)
    {
        return !still_wanted() || deliver(Err(DecodeFailure::Source(error)));
    }
    loop {
        if !still_wanted() {
            source.cancel_stream();
            return true;
        }
        let checkpoint = source.next_checkpoint(still_wanted);
        if !still_wanted() {
            // Cancelled inside the unit; whatever came back belongs to nobody.
            source.cancel_stream();
            return true;
        }
        let finished = !matches!(
            checkpoint,
            Ok(PositionedAlignmentStreamCheckpoint::Block(_))
        );
        if !deliver(checkpoint.map_err(DecodeFailure::Source)) {
            return false;
        }
        if finished {
            return true;
        }
    }
}

impl LocalBamCanonicalBackend {
    pub fn new(
        source: impl Into<ProductionAlignmentSource>,
        mut options: QueryOptions,
    ) -> Result<Self, LocalBamCanonicalBackendError> {
        let source = source.into();
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
            decoder: DecoderThread::spawn(source),
            source_revision,
            record_contract,
            options,
        })
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

    fn begin_or_retarget(
        &mut self,
        work: &SourceWork,
    ) -> Result<(), LocalBamCanonicalBackendError> {
        self.validate_work(work)?;
        if self.is_cram {
            // CRAM decoding is reference-based; the decode reference is fixed when the source
            // is opened, so a payload may name it without changing what is decoded.
        } else if work.intent().spec().payload().decode_reference().is_some() {
            return Err(LocalBamCanonicalBackendError::DecodeReferenceUnsupportedForBam);
        }
        let options = self.options_for(work.intent().spec().payload())?;
        self.decoder.begin(
            work.intent().spec().target(),
            work.intent().spec().admission_target(),
            options,
        )
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

impl CanonicalAlignmentSourceBackend for LocalBamCanonicalBackend {
    type Control = LocalBamSourceControl;
    type Error = LocalBamCanonicalBackendError;

    fn allocation_pressure(error: &Self::Error) -> Option<LedgerError> {
        match error {
            LocalBamCanonicalBackendError::Ledger(
                pressure @ (LedgerError::BudgetExceeded { .. }
                | LedgerError::TransientBudgetExceeded { .. }
                | LedgerError::ReplacementTransitionActive { .. }),
            ) => Some(*pressure),
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
        self.begin_or_retarget(work)
    }

    fn next_checkpoint(
        &mut self,
        work: &SourceWork,
        ledger: &AllocationLedger,
    ) -> Result<CanonicalDecodeCheckpoint, Self::Error> {
        self.validate_work(work)?;
        let Some(checkpoint) = self.decoder.next()? else {
            return Ok(CanonicalDecodeCheckpoint::Pending);
        };
        match checkpoint {
            PositionedAlignmentStreamCheckpoint::Block(block) => {
                let genomic_span = block
                    .genomic_span
                    .unwrap_or_else(|| work.intent().spec().target());
                if block.records.is_empty() {
                    Ok(CanonicalDecodeCheckpoint::Progress { genomic_span })
                } else {
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
                    Ok(CanonicalDecodeCheckpoint::Chunk {
                        chunk: self.pack_block(work, block, ledger)?,
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
        self.begin_or_retarget(work)
    }

    fn cancel(&mut self, _work: &SourceWork) {
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
        ) -> Result<(), AlignmentError> {
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

    fn fixture_backend(complete_read_limit: usize) -> (LocalBamCanonicalBackend, GenomicInterval) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let backend = LocalBamCanonicalBackend::new(
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
        let backend = LocalBamCanonicalBackend::new(source, QueryOptions::default()).unwrap();
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
        let backend = LocalBamCanonicalBackend::new(source, options.clone()).unwrap();
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
}
