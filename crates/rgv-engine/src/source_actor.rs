use std::{
    collections::{BTreeMap, VecDeque},
    num::NonZeroUsize,
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentSourceRevision, AllocationLedger, CompleteCoverage, ContractFingerprint,
    EvidenceAcquisition, EvidenceAcquisitionError, EvidenceChunk, EvidenceStore,
    EvidenceStoreError, LedgerError, PayloadContract, SourceChunkId,
};
use rgv_core::GenomicInterval;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceIntentSequence(u64);

impl SourceIntentSequence {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceWorkId(u64);

impl SourceWorkId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable scheduling owner for one panel/track alignment requirement. A source actor retains the
/// latest foreground intent independently for every live owner and rotates owners at canonical
/// source checkpoints.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceOwnerId(u64);

impl SourceOwnerId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIntentKind {
    Foreground,
    Speculative,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSourceIntentSpec {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    payload: PayloadContract,
    target: GenomicInterval,
}

impl AlignmentSourceIntentSpec {
    #[must_use]
    pub const fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        payload: PayloadContract,
        target: GenomicInterval,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            payload,
            target,
        }
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
    pub const fn payload(&self) -> &PayloadContract {
        &self.payload
    }

    #[must_use]
    pub const fn target(&self) -> GenomicInterval {
        self.target
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSourceIntent {
    owner: SourceOwnerId,
    sequence: SourceIntentSequence,
    spec: AlignmentSourceIntentSpec,
}

impl AlignmentSourceIntent {
    #[must_use]
    pub const fn owner(&self) -> SourceOwnerId {
        self.owner
    }

    #[must_use]
    pub const fn sequence(&self) -> SourceIntentSequence {
        self.sequence
    }

    #[must_use]
    pub const fn spec(&self) -> &AlignmentSourceIntentSpec {
        &self.spec
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceWork {
    id: SourceWorkId,
    kind: SourceIntentKind,
    intent: AlignmentSourceIntent,
}

impl SourceWork {
    #[must_use]
    pub const fn id(&self) -> SourceWorkId {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> SourceIntentKind {
        self.kind
    }

    #[must_use]
    pub const fn intent(&self) -> &AlignmentSourceIntent {
        &self.intent
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum AlignmentSourceActorAction<C> {
    Control(C),
    Work(SourceWork),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceYieldReason {
    NonDiscardableControl,
    ForegroundPreemption,
    ForegroundCancelled,
    IncompatibleRetarget,
    SpeculationReplaced,
    SpeculationCancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceCheckpointDecision {
    Continue(SourceWork),
    Retarget {
        previous: AlignmentSourceIntent,
        current: SourceWork,
    },
    Yield {
        work: SourceWork,
        reason: SourceYieldReason,
    },
}

/// One completed canonical source unit. Completeness remains transaction-scoped; this value is
/// only the independently salvageable chunk produced at a bounded source checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletedCanonicalChunk {
    pub id: SourceChunkId,
    pub source_revision: AlignmentSourceRevision,
    pub record_contract: ContractFingerprint,
    pub genomic_span: GenomicInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletedChunkDisposition {
    /// A successfully completed compatible chunk is admitted to the source cache even if the
    /// camera intent which initiated it is stale.
    pub admit_to_cache: bool,
    pub useful_foreground: Option<SourceIntentSequence>,
    pub useful_speculation: Option<SourceIntentSequence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedSourceWork {
    pub work: SourceWork,
    /// Only a foreground work item that still is the latest desired sequence may publish.
    pub publish: Option<SourceIntentSequence>,
}

/// One bounded result from a canonical decoder. The backend must reserve through the supplied
/// ledger before allocating a chunk. `Complete` certifies the entire transaction and never an
/// independently evictable member chunk.
#[derive(Debug)]
pub enum CanonicalDecodeCheckpoint {
    Chunk {
        chunk: Arc<EvidenceChunk>,
        genomic_span: GenomicInterval,
    },
    /// A physical source unit contained no eligible individual records, but still forms a
    /// mandatory preemption boundary for latest-wins scheduling.
    Progress {
        genomic_span: GenomicInterval,
    },
    Complete(CompleteCoverage),
    /// The full source population was evaluated exactly, but individual evidence exceeded the
    /// configured Complete admission policy. Previously completed chunks remain salvageable;
    /// this transaction must not publish an evidence root.
    Refused {
        coverage: CompleteCoverage,
        complete_read_limit: usize,
    },
}

/// Mutable source backend owned by exactly one [`AlignmentSourceActor`]. Implementations may keep
/// an indexed BAM/CRAM reader open, but must return after each canonical physical unit so the actor
/// can observe controls and newer desired targets.
pub trait CanonicalAlignmentSourceBackend {
    type Control;
    type Error;

    /// Returns recoverable allocation pressure carried by a backend-specific error. This keeps
    /// the generic actor independent of each decoder's error enum while allowing the engine to
    /// distinguish budget admission from source corruption or I/O failure.
    fn allocation_pressure(_error: &Self::Error) -> Option<LedgerError> {
        None
    }

    /// Whether this control changes the policy which previously produced an exact individual-read
    /// refusal. The threaded scene runtime uses this signal to retry refused foreground lanes
    /// exactly once after the non-discardable control has been queued. Decoder tuning controls do
    /// not cause an otherwise terminal refusal to restart.
    fn control_invalidates_read_refusals(_control: &Self::Control) -> bool {
        false
    }

    fn apply_control(&mut self, control: Self::Control) -> Result<(), Self::Error>;

    fn begin_work(
        &mut self,
        work: &SourceWork,
        ledger: &AllocationLedger,
    ) -> Result<(), Self::Error>;

    fn next_checkpoint(
        &mut self,
        work: &SourceWork,
        ledger: &AllocationLedger,
    ) -> Result<CanonicalDecodeCheckpoint, Self::Error>;

    fn retarget(&mut self, work: &SourceWork) -> Result<(), Self::Error>;

    fn cancel(&mut self, work: &SourceWork);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlignmentSourceActorEvent {
    ControlApplied,
    WorkStarted(SourceWork),
    ChunkAdmitted {
        id: SourceChunkId,
        disposition: CompletedChunkDisposition,
    },
    SourceProgressed {
        work: SourceWork,
        genomic_span: GenomicInterval,
    },
    WorkRetargeted {
        previous: AlignmentSourceIntent,
        current: SourceWork,
        salvaged_chunks: usize,
    },
    WorkYielded {
        work: SourceWork,
        reason: SourceYieldReason,
        salvaged_chunks: usize,
    },
    AcquisitionCompleted {
        work: SourceWork,
        admitted_revision: u64,
        publish: Option<SourceIntentSequence>,
    },
    AcquisitionRefused {
        work: SourceWork,
        coverage: CompleteCoverage,
        complete_read_limit: usize,
        publish: Option<SourceIntentSequence>,
    },
}

/// Single-owner source actor joining the latest-wins control plane to the immutable evidence
/// store. `pump_once` performs at most one control or one decoder checkpoint.
pub struct AlignmentSourceActor<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    schedule: AlignmentSourceActorSchedule<B::Control>,
    backend: B,
    store: EvidenceStore,
    transaction_chunks: Vec<Arc<EvidenceChunk>>,
}

impl<B> AlignmentSourceActor<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    #[must_use]
    pub fn new(
        backend: B,
        store: EvidenceStore,
        control_capacity: NonZeroUsize,
        maximum_control_burst: NonZeroUsize,
    ) -> Self {
        Self {
            schedule: AlignmentSourceActorSchedule::new(control_capacity, maximum_control_burst),
            backend,
            store,
            transaction_chunks: Vec::new(),
        }
    }

    pub fn submit_control(
        &mut self,
        control: B::Control,
    ) -> Result<(), SourceScheduleError<B::Control>> {
        self.schedule.submit_control(control)
    }

    pub fn submit_foreground(
        &mut self,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        self.schedule.submit_foreground(spec)
    }

    pub fn submit_foreground_for(
        &mut self,
        owner: SourceOwnerId,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        self.schedule.submit_foreground_for(owner, spec)
    }

    pub fn submit_speculation(
        &mut self,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        self.schedule.submit_speculation(spec)
    }

    pub fn submit_speculation_for(
        &mut self,
        owner: SourceOwnerId,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        self.schedule.submit_speculation_for(owner, spec)
    }

    pub fn cancel_foreground(&mut self) {
        self.schedule.cancel_foreground();
    }

    pub fn cancel_foreground_for(&mut self, owner: SourceOwnerId) {
        self.schedule.cancel_foreground_for(owner);
    }

    pub fn cancel_speculation(&mut self) {
        self.schedule.cancel_speculation();
    }

    pub fn cancel_speculation_for(&mut self, owner: SourceOwnerId) {
        self.schedule.cancel_speculation_for(owner);
    }

    /// Cancels a failed work item only when it is still the latest desired intent for its owner.
    ///
    /// A newer camera request can replace the desired intent before the decoder reaches its next
    /// bounded checkpoint. Resource-pressure handling must not turn a failure belonging to that
    /// stale transaction into cancellation of the newer request.
    pub fn cancel_failed_work_if_current(&mut self, work: &SourceWork) -> bool {
        match work.kind() {
            SourceIntentKind::Foreground => {
                let current = self
                    .schedule
                    .desired_foreground_for(work.intent().owner())
                    .is_some_and(|intent| intent.sequence() == work.intent().sequence());
                if current {
                    self.schedule.cancel_foreground_for(work.intent().owner());
                }
                current
            }
            SourceIntentKind::Speculative => {
                let current = self.schedule.desired_speculation().is_some_and(|intent| {
                    intent.owner() == work.intent().owner()
                        && intent.sequence() == work.intent().sequence()
                });
                if current {
                    self.schedule.cancel_speculation_for(work.intent().owner());
                }
                current
            }
        }
    }

    /// Cancels pending speculation and immediately abandons an active speculative decoder.
    ///
    /// Normal camera retargeting uses cooperative cancellation at the next canonical checkpoint
    /// so a just-completed source unit can still be salvaged. Resource-pressure handling cannot
    /// wait for another potentially allocating checkpoint, so it uses this single-owner fast
    /// path instead. Chunks admitted by earlier checkpoints remain independently cached; only the
    /// incomplete transaction certificate is discarded.
    pub fn abort_speculation(&mut self) -> Option<SourceWork> {
        self.schedule.cancel_speculation();
        let active_is_speculative = self
            .schedule
            .active()
            .is_some_and(|work| work.kind() == SourceIntentKind::Speculative);
        if !active_is_speculative {
            return None;
        }
        let work = self
            .schedule
            .abandon_active()
            .expect("an observed active speculative work item remains active");
        self.backend.cancel(&work);
        self.transaction_chunks.clear();
        Some(work)
    }

    /// Executes one bounded actor step. A returned `None` means there is currently no desired or
    /// non-discardable work.
    pub fn pump_once(
        &mut self,
    ) -> Result<Option<AlignmentSourceActorEvent>, AlignmentSourceActorError<B::Error>> {
        let result = self.pump_once_inner();
        if let Err(error) = result {
            // A decoder/store failure may occur after the mutable source advanced. Never retry
            // that half-consumed transaction in place: abandon it, retain only chunks already
            // admitted independently, and let the still-desired latest intent restart cleanly.
            let failed_work = self.schedule.abandon_active();
            if let Some(work) = &failed_work {
                self.backend.cancel(&work);
            }
            self.transaction_chunks.clear();
            return Err(match failed_work {
                Some(work) => error.with_work(work),
                None => error,
            });
        }
        result
    }

    fn pump_once_inner(
        &mut self,
    ) -> Result<Option<AlignmentSourceActorEvent>, AlignmentSourceActorError<B::Error>> {
        if self.schedule.active().is_none() {
            let Some(action) = self.schedule.next_action()? else {
                return Ok(None);
            };
            return match action {
                AlignmentSourceActorAction::Control(control) => {
                    self.backend
                        .apply_control(control)
                        .map_err(AlignmentSourceActorError::Backend)?;
                    Ok(Some(AlignmentSourceActorEvent::ControlApplied))
                }
                AlignmentSourceActorAction::Work(work) => {
                    self.transaction_chunks.clear();
                    self.backend
                        .begin_work(&work, &self.store.ledger())
                        .map_err(AlignmentSourceActorError::Backend)?;
                    Ok(Some(AlignmentSourceActorEvent::WorkStarted(work)))
                }
            };
        }

        let work = self
            .schedule
            .active()
            .cloned()
            .expect("active source work was checked");
        match self
            .backend
            .next_checkpoint(&work, &self.store.ledger())
            .map_err(AlignmentSourceActorError::Backend)?
        {
            CanonicalDecodeCheckpoint::Chunk {
                chunk,
                genomic_span,
            } => {
                let completed = CompletedCanonicalChunk {
                    id: chunk.chunk().id(),
                    source_revision: chunk.chunk().source_revision(),
                    record_contract: chunk.chunk().record_contract(),
                    genomic_span,
                };
                let disposition = self.schedule.completed_chunk(completed)?;
                let chunk = self.store.admit_chunk(chunk)?;
                if !self
                    .transaction_chunks
                    .iter()
                    .any(|candidate| candidate.chunk().id() == chunk.chunk().id())
                {
                    self.transaction_chunks.push(Arc::clone(&chunk));
                }
                match self.schedule.checkpoint(genomic_span)? {
                    SourceCheckpointDecision::Continue(_) => {
                        Ok(Some(AlignmentSourceActorEvent::ChunkAdmitted {
                            id: completed.id,
                            disposition,
                        }))
                    }
                    SourceCheckpointDecision::Retarget { previous, current } => {
                        let salvaged_chunks = self.transaction_chunks.len();
                        self.transaction_chunks.clear();
                        self.backend
                            .retarget(&current)
                            .map_err(AlignmentSourceActorError::Backend)?;
                        Ok(Some(AlignmentSourceActorEvent::WorkRetargeted {
                            previous,
                            current,
                            salvaged_chunks,
                        }))
                    }
                    SourceCheckpointDecision::Yield { work, reason } => {
                        let salvaged_chunks = self.transaction_chunks.len();
                        self.transaction_chunks.clear();
                        self.backend.cancel(&work);
                        Ok(Some(AlignmentSourceActorEvent::WorkYielded {
                            work,
                            reason,
                            salvaged_chunks,
                        }))
                    }
                }
            }
            CanonicalDecodeCheckpoint::Complete(coverage) => {
                let acquisition = Arc::new(EvidenceAcquisition::new(
                    coverage,
                    self.transaction_chunks.drain(..),
                )?);
                let admitted_revision = self.store.admit(acquisition)?;
                let completed = self.schedule.finish_active()?;
                Ok(Some(AlignmentSourceActorEvent::AcquisitionCompleted {
                    work: completed.work,
                    admitted_revision,
                    publish: completed.publish,
                }))
            }
            CanonicalDecodeCheckpoint::Progress { genomic_span } => {
                match self.schedule.checkpoint(genomic_span)? {
                    SourceCheckpointDecision::Continue(work) => {
                        Ok(Some(AlignmentSourceActorEvent::SourceProgressed {
                            work,
                            genomic_span,
                        }))
                    }
                    SourceCheckpointDecision::Retarget { previous, current } => {
                        let salvaged_chunks = self.transaction_chunks.len();
                        self.transaction_chunks.clear();
                        self.backend
                            .retarget(&current)
                            .map_err(AlignmentSourceActorError::Backend)?;
                        Ok(Some(AlignmentSourceActorEvent::WorkRetargeted {
                            previous,
                            current,
                            salvaged_chunks,
                        }))
                    }
                    SourceCheckpointDecision::Yield { work, reason } => {
                        let salvaged_chunks = self.transaction_chunks.len();
                        self.transaction_chunks.clear();
                        self.backend.cancel(&work);
                        Ok(Some(AlignmentSourceActorEvent::WorkYielded {
                            work,
                            reason,
                            salvaged_chunks,
                        }))
                    }
                }
            }
            CanonicalDecodeCheckpoint::Refused {
                coverage,
                complete_read_limit,
            } => {
                self.transaction_chunks.clear();
                let completed = self.schedule.finish_active()?;
                Ok(Some(AlignmentSourceActorEvent::AcquisitionRefused {
                    work: completed.work,
                    coverage,
                    complete_read_limit,
                    publish: completed.publish,
                }))
            }
        }
    }

    #[must_use]
    pub const fn schedule(&self) -> &AlignmentSourceActorSchedule<B::Control> {
        &self.schedule
    }

    #[must_use]
    pub const fn store(&self) -> &EvidenceStore {
        &self.store
    }

    pub const fn store_mut(&mut self) -> &mut EvidenceStore {
        &mut self.store
    }

    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }
}

#[derive(Debug, Error)]
pub enum AlignmentSourceActorError<E> {
    #[error("alignment source work failed: {source}")]
    Work {
        work: SourceWork,
        #[source]
        source: AlignmentSourceWorkError<E>,
    },
    #[error(transparent)]
    Schedule(#[from] SourceScheduleError<()>),
    #[error(transparent)]
    Store(#[from] EvidenceStoreError),
    #[error(transparent)]
    Acquisition(#[from] EvidenceAcquisitionError),
    #[error("alignment source backend failed: {0}")]
    Backend(#[source] E),
}

impl<E> AlignmentSourceActorError<E> {
    fn with_work(self, work: SourceWork) -> Self {
        let source = match self {
            Self::Schedule(error) => AlignmentSourceWorkError::Schedule(error),
            Self::Store(error) => AlignmentSourceWorkError::Store(error),
            Self::Acquisition(error) => AlignmentSourceWorkError::Acquisition(error),
            Self::Backend(error) => AlignmentSourceWorkError::Backend(error),
            Self::Work { .. } => return self,
        };
        Self::Work { work, source }
    }

    /// The bounded source transaction which failed, when the failure happened after scheduling.
    #[must_use]
    pub const fn work(&self) -> Option<&SourceWork> {
        match self {
            Self::Work { work, .. } => Some(work),
            Self::Schedule(_) | Self::Store(_) | Self::Acquisition(_) | Self::Backend(_) => None,
        }
    }

    /// Backend-specific cause, if this error originated in the decoder adapter.
    #[must_use]
    pub const fn backend_error(&self) -> Option<&E> {
        match self {
            Self::Work {
                source: AlignmentSourceWorkError::Backend(error),
                ..
            }
            | Self::Backend(error) => Some(error),
            Self::Work { .. } | Self::Schedule(_) | Self::Store(_) | Self::Acquisition(_) => None,
        }
    }
}

/// Concrete cause of a failure attributed to one bounded source transaction.
#[derive(Debug, Error)]
pub enum AlignmentSourceWorkError<E> {
    #[error(transparent)]
    Schedule(SourceScheduleError<()>),
    #[error(transparent)]
    Store(EvidenceStoreError),
    #[error(transparent)]
    Acquisition(EvidenceAcquisitionError),
    #[error("alignment source backend failed: {0}")]
    Backend(#[source] E),
}

/// Bounded latest-wins control plane for the single mutable alignment source owner.
///
/// The schedule stores the latest desired foreground target per live owner, one speculative
/// target, one active bounded work item, and a configured number of non-discardable controls.
/// Source I/O must return to `checkpoint` between canonical chunks, which makes preemption and
/// owner-fairness latency a property of the canonical unit rather than the length of a stale FIFO.
#[derive(Debug)]
pub struct AlignmentSourceActorSchedule<C> {
    controls: VecDeque<C>,
    control_capacity: NonZeroUsize,
    maximum_control_burst: NonZeroUsize,
    controls_since_work: usize,
    desired_foreground: BTreeMap<SourceOwnerId, ForegroundSlot>,
    desired_speculation: Option<AlignmentSourceIntent>,
    satisfied_foreground: BTreeMap<SourceOwnerId, SourceIntentSequence>,
    satisfied_speculation: Option<SourceIntentSequence>,
    active: Option<SourceWork>,
    next_intent_sequence: u64,
    next_fairness_age: u64,
    next_work_id: u64,
}

#[derive(Debug)]
struct ForegroundSlot {
    intent: AlignmentSourceIntent,
    fairness_age: u64,
}

impl<C> AlignmentSourceActorSchedule<C> {
    #[must_use]
    pub fn new(control_capacity: NonZeroUsize, maximum_control_burst: NonZeroUsize) -> Self {
        Self {
            controls: VecDeque::with_capacity(control_capacity.get()),
            control_capacity,
            maximum_control_burst,
            controls_since_work: 0,
            desired_foreground: BTreeMap::new(),
            desired_speculation: None,
            satisfied_foreground: BTreeMap::new(),
            satisfied_speculation: None,
            active: None,
            next_intent_sequence: 1,
            next_fairness_age: 1,
            next_work_id: 1,
        }
    }

    pub fn submit_control(&mut self, control: C) -> Result<(), SourceScheduleError<C>> {
        if self.controls.len() == self.control_capacity.get() {
            return Err(SourceScheduleError::ControlQueueFull(control));
        }
        self.controls.push_back(control);
        Ok(())
    }

    pub fn submit_foreground(
        &mut self,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        self.submit_foreground_for(SourceOwnerId::default(), spec)
    }

    pub fn submit_foreground_for(
        &mut self,
        owner: SourceOwnerId,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        let intent = self.next_intent(owner, spec)?;
        let sequence = intent.sequence;
        let preserve_age = self.desired_foreground.get(&owner).is_some_and(|slot| {
            self.satisfied_foreground.get(&owner) != Some(&slot.intent.sequence)
        });
        let fairness_age = if preserve_age {
            self.desired_foreground
                .get(&owner)
                .expect("a preserved foreground age has an existing slot")
                .fairness_age
        } else {
            self.take_fairness_age()?
        };
        self.desired_foreground.insert(
            owner,
            ForegroundSlot {
                intent,
                fairness_age,
            },
        );
        Ok(sequence)
    }

    pub fn submit_speculation(
        &mut self,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        self.submit_speculation_for(SourceOwnerId::default(), spec)
    }

    pub fn submit_speculation_for(
        &mut self,
        owner: SourceOwnerId,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<SourceIntentSequence, SourceScheduleError<()>> {
        let intent = self.next_intent(owner, spec)?;
        let sequence = intent.sequence;
        self.desired_speculation = Some(intent);
        Ok(sequence)
    }

    pub fn cancel_foreground(&mut self) {
        self.cancel_foreground_for(SourceOwnerId::default());
    }

    pub fn cancel_foreground_for(&mut self, owner: SourceOwnerId) {
        self.desired_foreground.remove(&owner);
        self.satisfied_foreground.remove(&owner);
    }

    pub fn cancel_speculation(&mut self) {
        self.desired_speculation = None;
        self.satisfied_speculation = None;
    }

    pub fn cancel_speculation_for(&mut self, owner: SourceOwnerId) {
        if self
            .desired_speculation
            .as_ref()
            .is_some_and(|intent| intent.owner() == owner)
        {
            self.cancel_speculation();
        }
    }

    fn next_intent(
        &mut self,
        owner: SourceOwnerId,
        spec: AlignmentSourceIntentSpec,
    ) -> Result<AlignmentSourceIntent, SourceScheduleError<()>> {
        let sequence = SourceIntentSequence(self.next_intent_sequence);
        self.next_intent_sequence = self
            .next_intent_sequence
            .checked_add(1)
            .ok_or(SourceScheduleError::SequenceOverflow(()))?;
        Ok(AlignmentSourceIntent {
            owner,
            sequence,
            spec,
        })
    }

    fn take_fairness_age(&mut self) -> Result<u64, SourceScheduleError<()>> {
        let age = self.next_fairness_age;
        self.next_fairness_age = self
            .next_fairness_age
            .checked_add(1)
            .ok_or(SourceScheduleError::SequenceOverflow(()))?;
        Ok(age)
    }

    /// Takes the next actor action. It returns `None` while bounded source work is active.
    pub fn next_action(
        &mut self,
    ) -> Result<Option<AlignmentSourceActorAction<C>>, SourceScheduleError<()>> {
        if self.active.is_some() {
            return Ok(None);
        }
        let foreground_pending = self.foreground_pending();
        if !self.controls.is_empty()
            && (!foreground_pending || self.controls_since_work < self.maximum_control_burst.get())
        {
            self.controls_since_work = self.controls_since_work.saturating_add(1);
            return Ok(self
                .controls
                .pop_front()
                .map(AlignmentSourceActorAction::Control));
        }
        if foreground_pending {
            let intent = self
                .oldest_pending_foreground()
                .map(|(_, slot)| slot.intent.clone())
                .expect("pending foreground has a desired intent");
            return self
                .begin_work(SourceIntentKind::Foreground, intent)
                .map(Some);
        }
        if let Some(control) = self.controls.pop_front() {
            self.controls_since_work = self.controls_since_work.saturating_add(1);
            return Ok(Some(AlignmentSourceActorAction::Control(control)));
        }
        if self.speculation_pending() {
            let intent = self
                .desired_speculation
                .clone()
                .expect("pending speculation has a desired intent");
            return self
                .begin_work(SourceIntentKind::Speculative, intent)
                .map(Some);
        }
        Ok(None)
    }

    fn begin_work(
        &mut self,
        kind: SourceIntentKind,
        intent: AlignmentSourceIntent,
    ) -> Result<AlignmentSourceActorAction<C>, SourceScheduleError<()>> {
        let id = SourceWorkId(self.next_work_id);
        self.next_work_id = self
            .next_work_id
            .checked_add(1)
            .ok_or(SourceScheduleError::SequenceOverflow(()))?;
        self.controls_since_work = 0;
        let work = SourceWork { id, kind, intent };
        self.active = Some(work.clone());
        Ok(AlignmentSourceActorAction::Work(work))
    }

    /// Reaches the mandatory boundary between canonical source units. A newer useful target can
    /// inherit the active work ID; incompatible work yields immediately at this checkpoint.
    pub fn checkpoint(
        &mut self,
        active_chunk_span: GenomicInterval,
    ) -> Result<SourceCheckpointDecision, SourceScheduleError<()>> {
        let active = self
            .active
            .clone()
            .ok_or(SourceScheduleError::NoActiveWork(()))?;
        if !self.controls.is_empty() {
            self.active = None;
            return Ok(SourceCheckpointDecision::Yield {
                work: active,
                reason: SourceYieldReason::NonDiscardableControl,
            });
        }
        let desired = match active.kind {
            SourceIntentKind::Foreground => self
                .desired_foreground
                .get(&active.intent.owner)
                .map(|slot| slot.intent.clone()),
            SourceIntentKind::Speculative if self.foreground_pending() => {
                self.active = None;
                return Ok(SourceCheckpointDecision::Yield {
                    work: active,
                    reason: SourceYieldReason::ForegroundPreemption,
                });
            }
            SourceIntentKind::Speculative => self.desired_speculation.clone(),
        };
        let Some(desired) = desired else {
            self.active = None;
            return Ok(SourceCheckpointDecision::Yield {
                work: active.clone(),
                reason: match active.kind {
                    SourceIntentKind::Foreground => SourceYieldReason::ForegroundCancelled,
                    SourceIntentKind::Speculative => SourceYieldReason::SpeculationCancelled,
                },
            });
        };
        // Foreground owners are fair at complete transaction boundaries. A BAM/CRAM decoder has
        // one mutable indexed stream; yielding it merely because another panel is waiting would
        // restart the first transaction at its beginning and can livelock two owners forever.
        // Speculation and stale work remain preemptible at every canonical checkpoint.
        if desired.sequence == active.intent.sequence {
            return Ok(SourceCheckpointDecision::Continue(active));
        }
        if decode_compatible(&active.intent.spec, &desired.spec)
            && desired.spec.target.overlaps(active_chunk_span)
        {
            let previous = active.intent;
            let current = SourceWork {
                id: active.id,
                kind: active.kind,
                intent: desired,
            };
            self.active = Some(current.clone());
            return Ok(SourceCheckpointDecision::Retarget { previous, current });
        }
        self.active = None;
        Ok(SourceCheckpointDecision::Yield {
            work: active.clone(),
            reason: match active.kind {
                SourceIntentKind::Foreground => SourceYieldReason::IncompatibleRetarget,
                SourceIntentKind::Speculative => SourceYieldReason::SpeculationReplaced,
            },
        })
    }

    /// Classifies a successfully decoded canonical chunk before any candidate-publication check.
    /// Compatible completed work is always salvageable into the actor-owned cache.
    pub fn completed_chunk(
        &self,
        chunk: CompletedCanonicalChunk,
    ) -> Result<CompletedChunkDisposition, SourceScheduleError<()>> {
        let active = self
            .active
            .as_ref()
            .ok_or(SourceScheduleError::NoActiveWork(()))?;
        if chunk.source_revision != active.intent.spec.source_revision
            || chunk.record_contract != active.intent.spec.record_contract
        {
            return Err(SourceScheduleError::CompletedChunkContractMismatch(()));
        }
        Ok(CompletedChunkDisposition {
            admit_to_cache: true,
            useful_foreground: self
                .desired_foreground
                .values()
                .filter(|slot| {
                    decode_compatible(&active.intent.spec, &slot.intent.spec)
                        && slot.intent.spec.target.overlaps(chunk.genomic_span)
                })
                .min_by_key(|slot| slot.fairness_age)
                .map(|slot| slot.intent.sequence),
            useful_speculation: self.desired_speculation.as_ref().and_then(|intent| {
                (decode_compatible(&active.intent.spec, &intent.spec)
                    && intent.spec.target.overlaps(chunk.genomic_span))
                .then_some(intent.sequence)
            }),
        })
    }

    /// Completes the active transaction. Publication permission is latest-sequence exact; stale
    /// work may already have contributed completed chunks to the cache but cannot replace current.
    pub fn finish_active(&mut self) -> Result<CompletedSourceWork, SourceScheduleError<()>> {
        let work = self
            .active
            .take()
            .ok_or(SourceScheduleError::NoActiveWork(()))?;
        let publish = match work.kind {
            SourceIntentKind::Foreground
                if self
                    .desired_foreground
                    .get(&work.intent.owner)
                    .map(|slot| slot.intent.sequence)
                    == Some(work.intent.sequence) =>
            {
                self.satisfied_foreground
                    .insert(work.intent.owner, work.intent.sequence);
                Some(work.intent.sequence)
            }
            SourceIntentKind::Speculative
                if self
                    .desired_speculation
                    .as_ref()
                    .map(|intent| intent.sequence)
                    == Some(work.intent.sequence) =>
            {
                self.satisfied_speculation = Some(work.intent.sequence);
                None
            }
            _ => None,
        };
        Ok(CompletedSourceWork { work, publish })
    }

    #[must_use]
    pub fn active(&self) -> Option<&SourceWork> {
        self.active.as_ref()
    }

    /// Drops the unfinished active transaction while preserving the latest desired slots. The
    /// actor uses this after any backend/store failure so the target can restart from a clean
    /// source transaction on the next pump.
    pub fn abandon_active(&mut self) -> Option<SourceWork> {
        self.active.take()
    }

    #[must_use]
    pub fn desired_foreground(&self) -> Option<&AlignmentSourceIntent> {
        self.desired_foreground_for(SourceOwnerId::default())
    }

    #[must_use]
    pub fn desired_foreground_for(&self, owner: SourceOwnerId) -> Option<&AlignmentSourceIntent> {
        self.desired_foreground.get(&owner).map(|slot| &slot.intent)
    }

    /// Whether this owner's exact latest foreground intent is queued or actively decoding. This
    /// is a constant-time liveness observation; it does not inspect evidence or cache records.
    #[must_use]
    pub fn foreground_pending_for(&self, owner: SourceOwnerId) -> bool {
        self.desired_foreground.get(&owner).is_some_and(|slot| {
            self.satisfied_foreground.get(&owner) != Some(&slot.intent.sequence)
        })
    }

    #[must_use]
    pub fn desired_speculation(&self) -> Option<&AlignmentSourceIntent> {
        self.desired_speculation.as_ref()
    }

    #[must_use]
    pub fn pending_slots(&self) -> usize {
        self.controls
            .len()
            .saturating_add(self.pending_foreground_count())
            .saturating_add(usize::from(self.speculation_pending()))
            .saturating_add(usize::from(self.active.is_some()))
    }

    #[must_use]
    pub fn control_len(&self) -> usize {
        self.controls.len()
    }

    fn foreground_pending(&self) -> bool {
        self.pending_foreground_count() > 0
    }

    fn pending_foreground_count(&self) -> usize {
        self.desired_foreground
            .iter()
            .filter(|(owner, slot)| {
                self.satisfied_foreground.get(owner) != Some(&slot.intent.sequence)
            })
            .count()
    }

    fn oldest_pending_foreground(&self) -> Option<(SourceOwnerId, &ForegroundSlot)> {
        self.desired_foreground
            .iter()
            .filter(|(owner, slot)| {
                self.satisfied_foreground.get(owner) != Some(&slot.intent.sequence)
            })
            .min_by_key(|(owner, slot)| (slot.fairness_age, **owner))
            .map(|(owner, slot)| (*owner, slot))
    }

    fn speculation_pending(&self) -> bool {
        self.desired_speculation
            .as_ref()
            .is_some_and(|intent| Some(intent.sequence) != self.satisfied_speculation)
    }
}

fn decode_compatible(
    active: &AlignmentSourceIntentSpec,
    desired: &AlignmentSourceIntentSpec,
) -> bool {
    active.source_revision == desired.source_revision
        && active.record_contract == desired.record_contract
        && active.payload.satisfies(&desired.payload)
}

#[derive(Debug, Error)]
pub enum SourceScheduleError<T> {
    #[error("the bounded non-discardable source-control queue is full")]
    ControlQueueFull(T),
    #[error("source intent/work sequence space was exhausted")]
    SequenceOverflow(T),
    #[error("the source actor has no active bounded work")]
    NoActiveWork(T),
    #[error("completed canonical chunk is incompatible with its active source work")]
    CompletedChunkContractMismatch(T),
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rgv_alignment_scene::{
        AllocationClass, AllocationLedger, CanonicalRecordChunk, CapabilityRequirement,
        CompleteCoverage, EvidenceChunk, EvidenceStore, FilteredRecordProvenance, ReservationKind,
        RevisionFingerprint, SourceFingerprint,
    };
    use rgv_core::ContigId;
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentRecord, AlignmentSourcePosition, SequenceData,
    };

    use super::*;

    fn revision(value: u64) -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, value),
        )
    }

    fn payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [],
            None,
        )
    }

    fn interval(start: u64, end: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(0), start, end).unwrap()
    }

    fn intent(start: u64, end: u64) -> AlignmentSourceIntentSpec {
        AlignmentSourceIntentSpec::new(
            revision(4),
            ContractFingerprint::new(9),
            payload(),
            interval(start, end),
        )
    }

    fn schedule() -> AlignmentSourceActorSchedule<u32> {
        AlignmentSourceActorSchedule::new(
            NonZeroUsize::new(8).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        )
    }

    fn evidence_chunk(
        ledger: &AllocationLedger,
        block: u64,
        start: u64,
        end: u64,
    ) -> Arc<EvidenceChunk> {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: format!("record-{block}").into_bytes(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((block << 16) | 1),
            )
            .unwrap();
        let chunk = CanonicalRecordChunk::new(
            revision(4),
            SourceChunkId::BamBlock {
                compressed_offset: block,
            },
            Arc::new(batch),
            ContractFingerprint::new(9),
        )
        .unwrap();
        let footprint = EvidenceChunk::footprint(&chunk);
        let mut core = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core = core.register(footprint.core_bytes, block).unwrap();
        let mut overlap = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap = overlap
            .register(footprint.overlap_index_bytes, block)
            .unwrap();
        Arc::new(EvidenceChunk::new(chunk, core, overlap).unwrap())
    }

    #[derive(Default)]
    struct MockBackend {
        steps: VecDeque<CanonicalDecodeCheckpoint>,
        begun: Vec<SourceWorkId>,
        retargeted: Vec<SourceWorkId>,
        cancelled: Vec<SourceWorkId>,
        controls: Vec<u32>,
    }

    impl CanonicalAlignmentSourceBackend for MockBackend {
        type Control = u32;
        type Error = &'static str;

        fn apply_control(&mut self, control: Self::Control) -> Result<(), Self::Error> {
            self.controls.push(control);
            Ok(())
        }

        fn begin_work(
            &mut self,
            work: &SourceWork,
            _ledger: &AllocationLedger,
        ) -> Result<(), Self::Error> {
            self.begun.push(work.id());
            Ok(())
        }

        fn next_checkpoint(
            &mut self,
            _work: &SourceWork,
            _ledger: &AllocationLedger,
        ) -> Result<CanonicalDecodeCheckpoint, Self::Error> {
            self.steps.pop_front().ok_or("missing mock checkpoint")
        }

        fn retarget(&mut self, work: &SourceWork) -> Result<(), Self::Error> {
            self.retargeted.push(work.id());
            Ok(())
        }

        fn cancel(&mut self, work: &SourceWork) {
            self.cancelled.push(work.id());
        }
    }

    fn actor(
        ledger: &AllocationLedger,
        steps: impl IntoIterator<Item = CanonicalDecodeCheckpoint>,
    ) -> AlignmentSourceActor<MockBackend> {
        let backend = MockBackend {
            steps: steps.into_iter().collect(),
            ..MockBackend::default()
        };
        let store = EvidenceStore::new(revision(4), ContractFingerprint::new(9), ledger.clone());
        AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(8).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        )
    }

    fn take_work(schedule: &mut AlignmentSourceActorSchedule<u32>) -> SourceWork {
        let AlignmentSourceActorAction::Work(work) = schedule.next_action().unwrap().unwrap()
        else {
            panic!("expected source work");
        };
        work
    }

    #[test]
    fn a_large_target_burst_keeps_only_latest_bounded_slots() {
        let mut schedule = schedule();
        let mut latest = None;
        for target in 0..100_000_u64 {
            latest = Some(
                schedule
                    .submit_foreground(intent(target, target + 10))
                    .unwrap(),
            );
            schedule
                .submit_speculation(intent(target + 10, target + 20))
                .unwrap();
        }

        assert_eq!(schedule.pending_slots(), 2);
        assert_eq!(schedule.control_len(), 0);
        let work = take_work(&mut schedule);
        assert_eq!(work.intent.sequence, latest.unwrap());
        assert_eq!(work.intent.spec.target, interval(99_999, 100_009));
    }

    #[test]
    fn foreground_owners_complete_in_fair_submission_order_without_restart_livelock() {
        let mut schedule = schedule();
        let owners = [
            SourceOwnerId::new(11),
            SourceOwnerId::new(22),
            SourceOwnerId::new(33),
        ];
        for (ordinal, owner) in owners.into_iter().enumerate() {
            schedule
                .submit_foreground_for(owner, intent(u64::try_from(ordinal).unwrap() * 100, 1_000))
                .unwrap();
        }
        let latest_first = schedule
            .submit_foreground_for(owners[0], intent(50, 1_000))
            .unwrap();

        let first = take_work(&mut schedule);
        assert_eq!(first.intent.owner(), owners[0]);
        assert_eq!(first.intent.sequence(), latest_first);
        assert!(matches!(
            schedule.checkpoint(interval(50, 60)).unwrap(),
            SourceCheckpointDecision::Continue(_)
        ));
        assert_eq!(
            schedule.finish_active().unwrap().publish,
            Some(latest_first)
        );

        let second = take_work(&mut schedule);
        assert_eq!(second.intent.owner(), owners[1]);
        assert!(matches!(
            schedule.checkpoint(interval(100, 110)).unwrap(),
            SourceCheckpointDecision::Continue(_)
        ));
        assert!(schedule.finish_active().unwrap().publish.is_some());
        let third = take_work(&mut schedule);
        assert_eq!(third.intent.owner(), owners[2]);
        assert!(matches!(
            schedule.checkpoint(interval(200, 210)).unwrap(),
            SourceCheckpointDecision::Continue(_)
        ));
        assert!(schedule.finish_active().unwrap().publish.is_some());
        assert!(schedule.next_action().unwrap().is_none());
        assert_eq!(schedule.pending_foreground_count(), 0);
    }

    #[test]
    fn foreground_preempts_speculation_at_the_next_chunk_checkpoint() {
        let mut schedule = schedule();
        schedule.submit_speculation(intent(0, 100)).unwrap();
        let speculation = take_work(&mut schedule);
        assert_eq!(speculation.kind, SourceIntentKind::Speculative);
        let foreground = schedule.submit_foreground(intent(500, 600)).unwrap();

        let decision = schedule.checkpoint(interval(0, 10)).unwrap();
        assert!(matches!(
            decision,
            SourceCheckpointDecision::Yield {
                reason: SourceYieldReason::ForegroundPreemption,
                ..
            }
        ));
        let work = take_work(&mut schedule);
        assert_eq!(work.kind, SourceIntentKind::Foreground);
        assert_eq!(work.intent.sequence, foreground);
    }

    #[test]
    fn compatible_useful_work_retargets_without_changing_work_identity() {
        let mut schedule = schedule();
        schedule.submit_foreground(intent(0, 100)).unwrap();
        let first = take_work(&mut schedule);
        let latest = schedule.submit_foreground(intent(50, 150)).unwrap();

        let SourceCheckpointDecision::Retarget { previous, current } =
            schedule.checkpoint(interval(50, 60)).unwrap()
        else {
            panic!("overlapping compatible chunk should retarget");
        };
        assert_eq!(previous.sequence, first.intent.sequence);
        assert_eq!(current.id, first.id);
        assert_eq!(current.intent.sequence, latest);

        schedule.submit_foreground(intent(500, 600)).unwrap();
        assert!(matches!(
            schedule.checkpoint(interval(50, 60)).unwrap(),
            SourceCheckpointDecision::Yield {
                reason: SourceYieldReason::IncompatibleRetarget,
                ..
            }
        ));
    }

    #[test]
    fn completed_stale_chunks_are_salvaged_but_only_latest_work_publishes() {
        let mut schedule = schedule();
        let old = schedule.submit_foreground(intent(0, 100)).unwrap();
        let work = take_work(&mut schedule);
        let latest = schedule.submit_foreground(intent(200, 300)).unwrap();
        let disposition = schedule
            .completed_chunk(CompletedCanonicalChunk {
                id: SourceChunkId::BamBlock {
                    compressed_offset: 7,
                },
                source_revision: revision(4),
                record_contract: ContractFingerprint::new(9),
                genomic_span: interval(0, 10),
            })
            .unwrap();
        assert!(disposition.admit_to_cache);
        assert_eq!(disposition.useful_foreground, None);
        let completed = schedule.finish_active().unwrap();
        assert_eq!(completed.work.id, work.id);
        assert_eq!(completed.work.intent.sequence, old);
        assert_eq!(completed.publish, None);

        let current = take_work(&mut schedule);
        assert_eq!(current.intent.sequence, latest);
        let completed = schedule.finish_active().unwrap();
        assert_eq!(completed.publish, Some(latest));
    }

    #[test]
    fn non_discardable_controls_are_bounded_and_fair_with_foreground() {
        let mut schedule = schedule();
        for control in 0..8 {
            schedule.submit_control(control).unwrap();
        }
        assert!(matches!(
            schedule.submit_control(9),
            Err(SourceScheduleError::ControlQueueFull(9))
        ));
        schedule.submit_foreground(intent(0, 100)).unwrap();

        assert_eq!(
            schedule.next_action().unwrap(),
            Some(AlignmentSourceActorAction::Control(0))
        );
        assert_eq!(
            schedule.next_action().unwrap(),
            Some(AlignmentSourceActorAction::Control(1))
        );
        assert_eq!(take_work(&mut schedule).kind, SourceIntentKind::Foreground);
    }

    #[test]
    fn actor_salvages_completed_chunks_when_a_new_target_cancels_the_transaction() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let first = evidence_chunk(&ledger, 7, 1, 10);
        let second = evidence_chunk(&ledger, 8, 20, 30);
        let mut actor = actor(
            &ledger,
            [
                CanonicalDecodeCheckpoint::Chunk {
                    chunk: first,
                    genomic_span: interval(0, 20),
                },
                CanonicalDecodeCheckpoint::Chunk {
                    chunk: second,
                    genomic_span: interval(20, 40),
                },
            ],
        );
        actor.submit_foreground(intent(0, 100)).unwrap();
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(_))
        ));
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::ChunkAdmitted { .. })
        ));
        actor.submit_foreground(intent(500, 600)).unwrap();
        let event = actor.pump_once().unwrap().unwrap();

        assert!(matches!(
            event,
            AlignmentSourceActorEvent::WorkYielded {
                reason: SourceYieldReason::IncompatibleRetarget,
                salvaged_chunks: 2,
                ..
            }
        ));
        assert_eq!(actor.store().chunk_count(), 2);
        assert_eq!(actor.store().acquisition_count(), 0);
        assert_eq!(actor.backend().cancelled.len(), 1);
    }

    #[test]
    fn actor_certifies_and_admits_only_a_complete_dependency_set() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let chunk = evidence_chunk(&ledger, 7, 1, 10);
        let target = interval(0, 100);
        let coverage = CompleteCoverage::new(
            revision(4),
            ContractFingerprint::new(9),
            target,
            FilteredRecordProvenance {
                records_seen: 1,
                eligible_records: 1,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let mut actor = actor(
            &ledger,
            [
                CanonicalDecodeCheckpoint::Chunk {
                    chunk,
                    genomic_span: target,
                },
                CanonicalDecodeCheckpoint::Complete(coverage),
            ],
        );
        let desired = actor.submit_foreground(intent(0, 100)).unwrap();
        actor.pump_once().unwrap();
        actor.pump_once().unwrap();
        let event = actor.pump_once().unwrap().unwrap();

        assert!(matches!(
            event,
            AlignmentSourceActorEvent::AcquisitionCompleted {
                publish: Some(sequence),
                ..
            } if sequence == desired
        ));
        assert_eq!(actor.store().chunk_count(), 1);
        assert_eq!(actor.store().acquisition_count(), 1);
        let candidate = actor
            .store_mut()
            .prepare(target, payload(), ReservationKind::Normal)
            .unwrap();
        assert_eq!(candidate.root().select(interval(1, 2)).unwrap().len(), 1);
    }

    #[test]
    fn actor_abandons_a_half_consumed_transaction_after_backend_failure() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let mut actor = actor(&ledger, []);
        actor.submit_foreground(intent(0, 100)).unwrap();

        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(_))
        ));
        assert!(matches!(
            actor.pump_once(),
            Err(AlignmentSourceActorError::Work {
                source: AlignmentSourceWorkError::Backend("missing mock checkpoint"),
                ..
            })
        ));
        assert!(actor.schedule().active().is_none());
        assert_eq!(actor.backend().cancelled.len(), 1);
        assert_eq!(actor.store().acquisition_count(), 0);

        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(_))
        ));
        assert_eq!(actor.backend().begun.len(), 2);
    }

    #[test]
    fn failed_work_cancellation_does_not_remove_a_newer_owner_intent() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let mut actor = actor(&ledger, []);
        actor
            .submit_foreground_for(SourceOwnerId::new(7), intent(0, 100))
            .unwrap();
        actor.pump_once().unwrap();
        let error = actor.pump_once().unwrap_err();
        let failed = error.work().unwrap().clone();

        let replacement = actor
            .submit_foreground_for(SourceOwnerId::new(7), intent(500, 600))
            .unwrap();
        assert!(!actor.cancel_failed_work_if_current(&failed));
        assert_eq!(
            actor
                .schedule()
                .desired_foreground_for(SourceOwnerId::new(7))
                .unwrap()
                .sequence(),
            replacement
        );
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(work))
                if work.intent().sequence() == replacement
        ));
    }

    #[test]
    fn failed_work_cancellation_removes_only_the_matching_intent() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let mut actor = actor(&ledger, []);
        actor
            .submit_speculation_for(SourceOwnerId::new(8), intent(0, 100))
            .unwrap();
        actor.pump_once().unwrap();
        let error = actor.pump_once().unwrap_err();
        let failed = error.work().unwrap().clone();

        assert!(actor.cancel_failed_work_if_current(&failed));
        assert!(actor.schedule().desired_speculation().is_none());
        assert_eq!(actor.pump_once().unwrap(), None);
    }

    #[test]
    fn resource_pressure_can_abort_speculation_without_a_decoder_checkpoint() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let mut actor = actor(&ledger, []);
        actor.submit_speculation(intent(0, 100)).unwrap();
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(ref work))
                if work.kind() == SourceIntentKind::Speculative
        ));

        let aborted = actor.abort_speculation().unwrap();
        assert_eq!(aborted.kind(), SourceIntentKind::Speculative);
        assert!(actor.schedule().active().is_none());
        assert!(actor.schedule().desired_speculation().is_none());
        assert_eq!(actor.backend().cancelled, vec![aborted.id()]);
        assert_eq!(actor.store().acquisition_count(), 0);
        assert_eq!(actor.pump_once().unwrap(), None);
    }
}
