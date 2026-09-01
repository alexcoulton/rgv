use std::{
    collections::BTreeMap,
    error::Error,
    num::NonZeroUsize,
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rgv_alignment_scene::{
    AlignmentSceneCamera, AlignmentSourceRevision, AllocationLedger, ContractFingerprint,
    EvidenceStore, LedgerError, PresentationContract, PresentationStorePressure, SceneRevision,
    StorePressure,
};
use rgv_core::ReferenceDictionary;
use rgv_hts::{
    AlignmentHeaderMetadata, AlignmentIndexOverview, AlignmentOpenOptions, AlignmentSource,
    LocalAlignmentSource, QueryOptions,
};

use crate::alignment_scene_coordinator::coordinator_budget_pressure;
use crate::{
    AlignmentCoordinatorEviction, AlignmentCoordinatorFrame, AlignmentCoordinatorPressure,
    AlignmentCoordinatorUpdate, AlignmentReferenceAuthority, AlignmentSceneBuildContract,
    AlignmentSceneCoordinatorConfigError, AlignmentSceneCoordinatorTelemetry,
    AlignmentScenePreparationPolicy, AlignmentSceneSourceCoordinator, AlignmentSourceActor,
    CanonicalAlignmentSourceBackend, EngineQueueError, LocalBamCanonicalBackend, SourceIntentKind,
    SourceOwnerId,
    mailbox::{CoalescingReceiver, CoalescingSender, TryMailboxSendError, bounded_coalescing},
};

const WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
const MAXIMUM_INPUT_BURST: usize = 32;
/// Maintenance deliberately yields after this many ownership removals. The value bounds work per
/// checkpoint; it is not a memory-policy constant and does not change which exact data is kept.
const MAXIMUM_RECLAIM_ENTRIES_PER_QUANTUM: usize = 256;
const SLOW_WORKER_PHASE: Duration = Duration::from_millis(25);

/// Identity of one desktop request transaction for an alignment-scene lane.
///
/// Camera sequence alone cannot distinguish two requests for the same locus and contract. This
/// token is echoed by every owner-scoped result so a caller can reject an event that was already
/// queued when a newer transaction superseded it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentSceneRequestToken(u64);

impl AlignmentSceneRequestToken {
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
pub enum AlignmentSceneEngineOperation {
    OpenSource,
    InsertLane,
    RemoveLane,
    UpdateCamera,
    ReplaceContract,
    InstallReferenceAuthority,
    SetPreparationPolicy,
    PrewarmPresentation,
    SetSourceControl,
    ReduceBudget,
    EvictInactive,
    PumpSource,
    PumpPresentation,
    RetryResourcePressure,
}

/// Why an in-flight inactive-cache reclaim ceased to own the worker. Supersession is terminal for
/// the old transaction: callers must never be left waiting for a completion that can no longer be
/// produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentInactiveEvictionSupersession {
    DisjointCamera,
    BudgetPolicyChange,
    CacheClear,
}

/// Coarse worker boundary used only for slow-phase diagnostics. It identifies which synchronous
/// owner operation withheld the mailbox without coupling the engine to desktop tracing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentSceneWorkerPhase {
    UpdateCamera,
    PublishFrame,
    PumpSource,
    PumpPresentation,
}

impl AlignmentSceneWorkerPhase {
    const fn diagnostic_key(self) -> u64 {
        match self {
            Self::UpdateCamera => 10,
            Self::PublishFrame => 11,
            Self::PumpSource => 12,
            Self::PumpPresentation => 13,
        }
    }
}

/// Whether the failed allocation can become admissible without changing the scene contract.
/// Retryable pressure is a nonterminal scheduling edge; an infeasible request is still reported
/// through the typed event so callers can explain the exact budget mismatch rather than treating
/// the source backend as unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentSceneResourcePressureDisposition {
    Retryable,
    Infeasible,
}

impl AlignmentSceneResourcePressureDisposition {
    #[must_use]
    pub const fn for_pressure(pressure: LedgerError) -> Self {
        if pressure.intrinsically_exceeds_limit() {
            Self::Infeasible
        } else {
            Self::Retryable
        }
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentSceneSourceMetadata {
    pub dictionary: ReferenceDictionary,
    pub header: Arc<AlignmentHeaderMetadata>,
    pub index_overview: Option<Arc<AlignmentIndexOverview>>,
    pub source_revision: AlignmentSourceRevision,
    pub record_contract: ContractFingerprint,
    pub normalization_mapped_records: Option<u64>,
}

/// Everything needed to bootstrap one local BAM source without touching the caller thread.
/// Queue and allocation bounds are supplied explicitly; the runtime adds no hidden cache clamp.
#[derive(Clone, Debug)]
pub struct LocalAlignmentSceneEngineConfig {
    pub path: PathBuf,
    pub open_options: AlignmentOpenOptions,
    pub query_options: QueryOptions,
    /// Workspace-shared enforcing ledger. Multiple sources and the GPU renderer must receive
    /// clones of this same authority so the selected budget cannot be multiplied per track.
    pub ledger: AllocationLedger,
    pub mailbox_control_capacity: NonZeroUsize,
    pub source_control_capacity: NonZeroUsize,
    pub maximum_source_control_burst: NonZeroUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentSceneFrameCause {
    Camera,
    Contract,
    ReferenceAuthority,
    PreparationPolicy,
    SourceControlRetry,
    ResourcePressureRetry,
    SourcePublication,
    /// Latest-wins publication for a vertical row-camera change; source and atlas authority are
    /// unchanged.
    RowCamera,
    VerticalResidency,
}

/// Events crossing from the one source worker to the desktop. Published frames retain immutable
/// scene roots and can therefore be consumed without locking the source actor or its caches.
#[derive(Clone, Debug)]
pub enum AlignmentSceneEngineEvent {
    SourceOpened {
        metadata: AlignmentSceneSourceMetadata,
    },
    LaneInserted {
        owner: SourceOwnerId,
    },
    LaneRemoved {
        owner: SourceOwnerId,
        existed: bool,
    },
    FrameUpdated {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        cause: AlignmentSceneFrameCause,
        update: Option<AlignmentCoordinatorUpdate>,
        published_scene: Option<SceneRevision>,
        frame: AlignmentCoordinatorFrame,
        /// Exact bounded liveness ownership after producing this frame.
        work: crate::AlignmentSceneOwnerWorkState,
        telemetry: AlignmentSceneCoordinatorTelemetry,
    },
    /// The current exact foreground target was counted completely, but individual-read evidence
    /// exceeded the configured admission limit. `frame` may retain an older truthfully covered
    /// scene edge; this event is terminal for the requested target until camera or policy changes.
    IndividualReadsRefused {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        target: rgv_core::GenomicInterval,
        complete_read_limit: usize,
        eligible_records: u64,
        frame: AlignmentCoordinatorFrame,
        telemetry: AlignmentSceneCoordinatorTelemetry,
    },
    PresentationPrewarmed {
        owner: SourceOwnerId,
        prepared: bool,
    },
    SourceControlQueued,
    BudgetReduced {
        pressure: AlignmentCoordinatorPressure,
    },
    /// An eviction-only answer from one source/semantic owner participating in an aggregate
    /// foreground reclaim. The configured ledger policy is unchanged.
    InactiveEvicted {
        request_id: u64,
        eviction: AlignmentCoordinatorEviction,
    },
    /// One bounded maintenance checkpoint. This is diagnostic progress, not a terminal answer;
    /// the transaction still ends in `InactiveEvicted` or `InactiveEvictionSuperseded`.
    InactiveEvictionProgress {
        request_id: u64,
        eviction: AlignmentCoordinatorEviction,
    },
    /// The demand or budget authority changed before the old reclaim completed. Ownership progress
    /// is retained in the shared ledger and reported so aggregate callers can settle the old ID.
    InactiveEvictionSuperseded {
        request_id: u64,
        target_charged: usize,
        released_bytes: usize,
        relinquished_cache_entries: usize,
        reason: AlignmentInactiveEvictionSupersession,
    },
    /// A normally synchronous worker boundary exceeded the diagnostic duration threshold. Events
    /// are replaceable by phase and therefore cannot backpressure navigation.
    WorkerPhaseCompleted {
        phase: AlignmentSceneWorkerPhase,
        owner: Option<SourceOwnerId>,
        elapsed_nanoseconds: u64,
    },
    /// A source-local budget reduction was temporarily blocked by the workspace-shared ledger's
    /// prepared GPU replacement. No budget was reported as applied and no lane/source is failed.
    /// The requested value plus the observed policy identity let the desktop discard stale edits
    /// and retry the latest value only after the obstruction or shared policy changes.
    BudgetPressure {
        /// Final configured base budget. For a clear transaction this remains the restore value,
        /// so stale-event matching does not confuse its temporary zero pass with a settings edit.
        requested_budget: usize,
        /// Whether retry must still execute the ordered `0 -> requested_budget` cache-clear pass.
        clear_before_apply: bool,
        current_policy_revision: u64,
        current_budget: usize,
        pressure: LedgerError,
    },
    /// An owner-local source or scene-root allocation could not proceed under the shared ledger.
    /// This is never a source-availability error. For `Retryable`, the failed intent is paused and
    /// resumes only through `try_retry_resource_pressure`, after the caller observes capacity
    /// progress. The request token and target make stale retry notifications rejectable.
    ResourcePressure {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        target: rgv_core::GenomicInterval,
        /// Coordinator boundary which encountered the pressure. Source intent alone cannot
        /// distinguish camera/contract/reference/presentation retries.
        operation: AlignmentSceneEngineOperation,
        intent: SourceIntentKind,
        pressure: LedgerError,
        disposition: AlignmentSceneResourcePressureDisposition,
    },
    Error {
        owner: Option<SourceOwnerId>,
        request_token: Option<AlignmentSceneRequestToken>,
        operation: AlignmentSceneEngineOperation,
        message: Arc<str>,
    },
}

enum AlignmentSceneEngineRequest<C> {
    InsertLane {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        contract: AlignmentSceneBuildContract,
        policy: AlignmentScenePreparationPolicy,
    },
    RemoveLane {
        owner: SourceOwnerId,
    },
    UpdateCamera {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        camera: AlignmentSceneCamera,
    },
    SetOptionalWorkPaused {
        owner: SourceOwnerId,
        paused: bool,
    },
    ReplaceContract {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        contract: AlignmentSceneBuildContract,
    },
    InstallReferenceAuthority {
        owner: SourceOwnerId,
        authority: AlignmentReferenceAuthority,
    },
    SetPreparationPolicy {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        policy: AlignmentScenePreparationPolicy,
    },
    PrewarmPresentation {
        owner: SourceOwnerId,
        presentation: PresentationContract,
    },
    SetSourceControl(C),
    ReduceBudget {
        budget: usize,
    },
    EvictInactive {
        request_id: u64,
        target_charged: usize,
    },
    ClearCacheBudget {
        restore_budget: usize,
    },
    RetryResourcePressure {
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        target: rgv_core::GenomicInterval,
    },
}

/// Bounded background driver for one source-level coordinator. Controls are lossless and
/// capacity-bounded; camera requests are replaceable per owner. The UI owns only this mailbox,
/// immutable frame snapshots, and the shared allocation ledger—not the mutable decoder.
pub struct AlignmentSceneEngine<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    input: Option<CoalescingSender<AlignmentSceneEngineRequest<B::Control>>>,
    output: CoalescingReceiver<AlignmentSceneEngineEvent>,
    worker: Option<JoinHandle<()>>,
    ledger: AllocationLedger,
}

impl<B> AlignmentSceneEngine<B>
where
    B: CanonicalAlignmentSourceBackend + Send + 'static,
    B::Control: Send + 'static,
    B::Error: Error + Send + Sync + 'static,
{
    #[must_use]
    pub fn spawn(coordinator: AlignmentSceneSourceCoordinator<B>, control_capacity: usize) -> Self {
        let ledger = coordinator.actor().store().ledger();
        let (input_tx, input_rx) = bounded_coalescing(control_capacity);
        let (output_tx, output_rx) = bounded_coalescing(control_capacity);
        let worker = thread::Builder::new()
            .name("rgv-alignment-scene-source".to_owned())
            .spawn(move || scene_worker_loop(coordinator, &input_rx, &output_tx))
            .expect("failed to spawn RGV unified alignment scene worker");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
            ledger,
        }
    }

    pub fn try_insert_lane(
        &self,
        owner: SourceOwnerId,
        contract: AlignmentSceneBuildContract,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<(), EngineQueueError> {
        self.try_insert_lane_for_request(
            owner,
            AlignmentSceneRequestToken::default(),
            contract,
            policy,
        )
    }

    pub fn try_insert_lane_for_request(
        &self,
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        contract: AlignmentSceneBuildContract,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::InsertLane {
            owner,
            request_token,
            contract,
            policy,
        })
    }

    pub fn try_remove_lane(&self, owner: SourceOwnerId) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::RemoveLane { owner })
    }

    /// Replaces any unread camera intent for this owner. It never waits for the source worker and
    /// cannot create a pointer-event FIFO.
    pub fn try_update_camera(
        &self,
        owner: SourceOwnerId,
        camera: impl Into<AlignmentSceneCamera>,
    ) -> Result<(), EngineQueueError> {
        self.try_update_camera_for_request(owner, AlignmentSceneRequestToken::default(), camera)
    }

    /// Replaces any unread camera intent for this owner and associates every resulting event with
    /// the caller's exact request transaction.
    pub fn try_update_camera_for_request(
        &self,
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        camera: impl Into<AlignmentSceneCamera>,
    ) -> Result<(), EngineQueueError> {
        let camera = camera.into();
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .send_latest_for(
                owner.get(),
                AlignmentSceneEngineRequest::UpdateCamera {
                    owner,
                    request_token,
                    camera,
                },
            )
            .map_err(|_| EngineQueueError::Disconnected)
    }

    /// Pauses/resumes optional vertical semantic-page preparation for one visible owner. This is
    /// a replaceable foreground-state edge, not an ordered command: a burst can retain only its
    /// newest state and the worker observes it at the next canonical block checkpoint.
    pub fn try_set_optional_work_paused(
        &self,
        owner: SourceOwnerId,
        paused: bool,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .send_latest_system_for(
                3,
                owner.get(),
                AlignmentSceneEngineRequest::SetOptionalWorkPaused { owner, paused },
            )
            .map_err(|_| EngineQueueError::Disconnected)
    }

    pub fn try_replace_contract(
        &self,
        owner: SourceOwnerId,
        contract: AlignmentSceneBuildContract,
    ) -> Result<(), EngineQueueError> {
        self.try_replace_contract_for_request(
            owner,
            AlignmentSceneRequestToken::default(),
            contract,
        )
    }

    pub fn try_replace_contract_for_request(
        &self,
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        contract: AlignmentSceneBuildContract,
    ) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::ReplaceContract {
            owner,
            request_token,
            contract,
        })
    }

    /// Offers an immutable FASTA slice to one lane. The worker accepts it only when its revision
    /// and coverage match that lane's exact live requirement; stale or short slices are harmless
    /// no-ops inside the coordinator.
    pub fn try_install_reference_authority(
        &self,
        owner: SourceOwnerId,
        authority: AlignmentReferenceAuthority,
    ) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::InstallReferenceAuthority {
            owner,
            authority,
        })
    }

    pub fn try_set_preparation_policy(
        &self,
        owner: SourceOwnerId,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<(), EngineQueueError> {
        self.try_set_preparation_policy_for_request(
            owner,
            AlignmentSceneRequestToken::default(),
            policy,
        )
    }

    pub fn try_set_preparation_policy_for_request(
        &self,
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::SetPreparationPolicy {
            owner,
            request_token,
            policy,
        })
    }

    pub fn try_prewarm_presentation(
        &self,
        owner: SourceOwnerId,
        presentation: PresentationContract,
    ) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::PrewarmPresentation {
            owner,
            presentation,
        })
    }

    pub fn try_set_source_control(&self, control: B::Control) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::SetSourceControl(control))
    }

    pub fn try_reduce_budget(&self, budget: usize) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::ReduceBudget { budget })
    }

    /// Asks this source/semantic owner to yield inactive cache capacity without mutating the
    /// workspace budget. Foreground GPU admission calls this across every registered engine.
    pub fn try_evict_inactive_for_headroom(
        &self,
        request_id: u64,
        target_charged: usize,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .send_latest_system(
                2,
                AlignmentSceneEngineRequest::EvictInactive {
                    request_id,
                    target_charged,
                },
            )
            .map_err(|_| EngineQueueError::Disconnected)
    }

    /// Runs the cache-clear budget pulse as one ordered worker transaction. It must not be
    /// represented as two latest-wins edits: coalescing `0 -> configured` would silently omit the
    /// eviction pass which the user explicitly requested.
    pub fn try_clear_cache_budget(&self, restore_budget: usize) -> Result<(), EngineQueueError> {
        self.try_control(AlignmentSceneEngineRequest::ClearCacheBudget { restore_budget })
    }

    /// Retries a previously refused policy edit through a source-wide latest-wins slot. Ordinary
    /// budget controls remain ordered so the Clear Cache `0 -> configured budget` transaction is
    /// never collapsed; only retries are replaceable, and therefore cannot fill the control FIFO.
    pub fn try_retry_budget_pressure(
        &self,
        budget: usize,
        clear_before_apply: bool,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        let request = if clear_before_apply {
            AlignmentSceneEngineRequest::ClearCacheBudget {
                restore_budget: budget,
            }
        } else {
            AlignmentSceneEngineRequest::ReduceBudget { budget }
        };
        input
            .send_latest_system(0, request)
            .map_err(|_| EngineQueueError::Disconnected)
    }

    /// Resumes a pressure-paused source intent without reconstructing or advancing its camera.
    /// The replaceable mailbox and worker-side token/target checks make a retry for a superseded
    /// desktop transaction a harmless no-op.
    pub fn try_retry_resource_pressure(
        &self,
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        target: rgv_core::GenomicInterval,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        // A retry must never replace an unread newer camera and must not strand behind a full
        // control FIFO. Its dedicated owner/system slot is bounded, latest-wins, and token-checked
        // by the worker after dequeue.
        input
            .send_latest_system_for(
                1,
                owner.get(),
                AlignmentSceneEngineRequest::RetryResourcePressure {
                    owner,
                    request_token,
                    target,
                },
            )
            .map_err(|_| EngineQueueError::Disconnected)
    }

    pub fn try_receive(&self) -> Result<Option<AlignmentSceneEngineEvent>, EngineQueueError> {
        self.output
            .try_receive()
            .map_err(|_| EngineQueueError::Disconnected)
    }

    pub fn receive_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<AlignmentSceneEngineEvent>, EngineQueueError> {
        self.output
            .receive_timeout(timeout)
            .map_err(|_| EngineQueueError::Disconnected)
    }

    #[must_use]
    pub const fn ledger(&self) -> &AllocationLedger {
        &self.ledger
    }

    /// Closes the worker without joining on the caller, matching desktop track-removal semantics.
    #[must_use]
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.input.take();
        self.output.close();
        let worker = self.worker.take()?;
        match thread::Builder::new()
            .name("rgv-alignment-scene-reaper".to_owned())
            .spawn(move || {
                if worker.join().is_err() {
                    tracing::warn!("unified alignment scene worker panicked while retiring");
                }
            }) {
            Ok(reaper) => Some(reaper),
            Err(error) => {
                tracing::warn!(%error, "could not spawn unified scene reaper; worker detached");
                None
            }
        }
    }

    fn try_control(
        &self,
        request: AlignmentSceneEngineRequest<B::Control>,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send_control(request)
            .map_err(|error| match error {
                TryMailboxSendError::Full(_) => EngineQueueError::Full,
                TryMailboxSendError::Disconnected(_) => EngineQueueError::Disconnected,
            })
    }
}

impl AlignmentSceneEngine<LocalBamCanonicalBackend> {
    /// Opens and owns a local BAM entirely on the unified scene source thread. The caller can
    /// immediately retain the enforcing ledger and mailbox; source metadata arrives through
    /// `SourceOpened` before any lane can be installed.
    #[must_use]
    pub fn spawn_local_bam(config: LocalAlignmentSceneEngineConfig) -> Self {
        let ledger = config.ledger.clone();
        let (input_tx, input_rx) = bounded_coalescing(config.mailbox_control_capacity.get());
        let (output_tx, output_rx) = bounded_coalescing(config.mailbox_control_capacity.get());
        let worker_ledger = ledger.clone();
        let worker = thread::Builder::new()
            .name("rgv-local-bam-scene-source".to_owned())
            .spawn(move || {
                let source = match LocalAlignmentSource::open(&config.path, &config.open_options) {
                    Ok(source) => source,
                    Err(error) => {
                        send_error(
                            &output_tx,
                            None,
                            None,
                            AlignmentSceneEngineOperation::OpenSource,
                            error,
                        );
                        return;
                    }
                };
                let dictionary = source.reference_dictionary().clone();
                let index_overview = source.index_overview().cloned().map(Arc::new);
                let header = Arc::clone(source.header_metadata());
                let normalization_mapped_records = source.normalization_mapped_records();
                let backend = match LocalBamCanonicalBackend::new(source, config.query_options) {
                    Ok(backend) => backend,
                    Err(error) => {
                        send_error(
                            &output_tx,
                            None,
                            None,
                            AlignmentSceneEngineOperation::OpenSource,
                            error,
                        );
                        return;
                    }
                };
                let source_revision = backend.source_revision();
                let record_contract = backend.record_contract();
                let store = EvidenceStore::new(source_revision, record_contract, worker_ledger);
                let actor = AlignmentSourceActor::new(
                    backend,
                    store,
                    config.source_control_capacity,
                    config.maximum_source_control_burst,
                );
                if !send_control(
                    &output_tx,
                    AlignmentSceneEngineEvent::SourceOpened {
                        metadata: AlignmentSceneSourceMetadata {
                            dictionary,
                            header,
                            index_overview,
                            source_revision,
                            record_contract,
                            normalization_mapped_records,
                        },
                    },
                ) {
                    return;
                }
                scene_worker_loop(
                    AlignmentSceneSourceCoordinator::new(actor),
                    &input_rx,
                    &output_tx,
                );
            })
            .expect("failed to spawn RGV local BAM unified scene worker");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
            ledger,
        }
    }
}

impl<B> Drop for AlignmentSceneEngine<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    fn drop(&mut self) {
        self.input.take();
        self.output.close();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("unified alignment scene worker panicked while shutting down");
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingInactiveEviction {
    request_id: u64,
    target_charged: usize,
    accumulated: Option<AlignmentCoordinatorEviction>,
}

fn report_slow_worker_phase(
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    phase: AlignmentSceneWorkerPhase,
    owner: Option<SourceOwnerId>,
    started: Instant,
) -> bool {
    let elapsed = started.elapsed();
    if elapsed < SLOW_WORKER_PHASE {
        return true;
    }
    output
        .send_latest_system(
            phase.diagnostic_key(),
            AlignmentSceneEngineEvent::WorkerPhaseCompleted {
                phase,
                owner,
                elapsed_nanoseconds: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            },
        )
        .is_ok()
}

fn supersede_pending_eviction(
    pending_eviction: &mut Option<PendingInactiveEviction>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    reason: AlignmentInactiveEvictionSupersession,
) -> bool {
    let Some(pending) = pending_eviction.take() else {
        return true;
    };
    let released_bytes = pending.accumulated.map_or(0, |eviction| eviction.released);
    let relinquished_cache_entries = pending
        .accumulated
        .map_or(0, |eviction| eviction.relinquished_cache_entries);
    send_control(
        output,
        AlignmentSceneEngineEvent::InactiveEvictionSuperseded {
            request_id: pending.request_id,
            target_charged: pending.target_charged,
            released_bytes,
            relinquished_cache_entries,
            reason,
        },
    )
}

fn merge_inactive_eviction(
    previous: Option<AlignmentCoordinatorEviction>,
    next: AlignmentCoordinatorEviction,
) -> AlignmentCoordinatorEviction {
    let Some(previous) = previous else {
        return next;
    };
    AlignmentCoordinatorEviction {
        target_charged: next.target_charged,
        speculation_was_pending: previous.speculation_was_pending || next.speculation_was_pending,
        active_speculation_aborted: previous.active_speculation_aborted
            || next.active_speculation_aborted,
        presentation: PresentationStorePressure {
            budget: next.presentation.budget,
            evicted_read_pages: previous
                .presentation
                .evicted_read_pages
                .saturating_add(next.presentation.evicted_read_pages),
            evicted_summary_pages: previous
                .presentation
                .evicted_summary_pages
                .saturating_add(next.presentation.evicted_summary_pages),
            candidates_inspected: previous
                .presentation
                .candidates_inspected
                .saturating_add(next.presentation.candidates_inspected),
            reclaim_complete: next.presentation.reclaim_complete,
            residency: next.presentation.residency,
            charged: next.presentation.charged,
            pinned: next.presentation.pinned,
            pinned_overage: next.presentation.pinned_overage,
            remaining_overage: next.presentation.remaining_overage,
        },
        evidence: StorePressure {
            evicted_acquisitions: previous
                .evidence
                .evicted_acquisitions
                .saturating_add(next.evidence.evicted_acquisitions),
            evicted_chunks: previous
                .evidence
                .evicted_chunks
                .saturating_add(next.evidence.evicted_chunks),
            evicted_exact_summaries: previous
                .evidence
                .evicted_exact_summaries
                .saturating_add(next.evidence.evicted_exact_summaries),
            evicted_row_atlases: previous
                .evidence
                .evicted_row_atlases
                .saturating_add(next.evidence.evicted_row_atlases),
            candidates_inspected: previous
                .evidence
                .candidates_inspected
                .saturating_add(next.evidence.candidates_inspected),
            reclaim_complete: next.evidence.reclaim_complete,
            residency: next.evidence.residency,
            charged: next.evidence.charged,
            pinned: next.evidence.pinned,
            pinned_overage: next.evidence.pinned_overage,
        },
        charged_before: previous.charged_before,
        charged: next.charged,
        pinned: next.pinned,
        released: previous.charged_before.saturating_sub(next.charged),
        relinquished_cache_entries: previous
            .relinquished_cache_entries
            .saturating_add(next.relinquished_cache_entries),
        remaining: next.remaining,
        quantum_entries: previous
            .quantum_entries
            .saturating_add(next.quantum_entries),
        quanta: previous.quanta.saturating_add(next.quanta),
        elapsed_nanoseconds: previous
            .elapsed_nanoseconds
            .saturating_add(next.elapsed_nanoseconds),
        reclaim_complete: next.reclaim_complete,
    }
}

fn scene_worker_loop<B>(
    mut coordinator: AlignmentSceneSourceCoordinator<B>,
    input: &CoalescingReceiver<AlignmentSceneEngineRequest<B::Control>>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
) where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + Send + Sync + 'static,
{
    let mut request_tokens = BTreeMap::new();
    let mut pending_eviction = None;
    loop {
        let first = if coordinator.has_pending_source_work()
            || coordinator.has_pending_presentation_work()
            || pending_eviction.is_some()
        {
            input.try_receive()
        } else {
            input.receive_timeout(WORKER_IDLE_WAIT)
        };
        match first {
            Ok(Some(request)) => {
                if !apply_request(
                    &mut coordinator,
                    &mut request_tokens,
                    &mut pending_eviction,
                    output,
                    request,
                ) {
                    return;
                }
                for _ in 1..MAXIMUM_INPUT_BURST {
                    match input.try_receive() {
                        Ok(Some(request)) => {
                            if !apply_request(
                                &mut coordinator,
                                &mut request_tokens,
                                &mut pending_eviction,
                                output,
                                request,
                            ) {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => return,
                    }
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }

        // Reclaim yields to the mailbox between bounded quanta, but not to foreground allocation.
        // Letting one page build between quanta consumed freshly released bytes and made a large
        // low-water target mathematically unreachable—the exact saturation loop this worker is
        // responsible for breaking.
        let run_maintenance = pending_eviction.is_some();
        if run_maintenance {
            let mut pending = pending_eviction
                .take()
                .expect("the selected maintenance turn still has a reclaim transaction");
            match coordinator
                .evict_inactive_batch(pending.target_charged, MAXIMUM_RECLAIM_ENTRIES_PER_QUANTUM)
            {
                Ok(eviction) => {
                    if output
                        .send_latest_system(
                            0,
                            AlignmentSceneEngineEvent::InactiveEvictionProgress {
                                request_id: pending.request_id,
                                eviction,
                            },
                        )
                        .is_err()
                    {
                        return;
                    }
                    let stopped_without_progress = eviction.quantum_entries == 0;
                    let complete = eviction.reclaim_complete || stopped_without_progress;
                    pending.accumulated =
                        Some(merge_inactive_eviction(pending.accumulated, eviction));
                    if complete {
                        let eviction = pending
                            .accumulated
                            .expect("a completed reclaim has at least one bounded result");
                        if !send_control(
                            output,
                            AlignmentSceneEngineEvent::InactiveEvicted {
                                request_id: pending.request_id,
                                eviction,
                            },
                        ) {
                            return;
                        }
                    } else {
                        pending_eviction = Some(pending);
                    }
                }
                Err(error) => {
                    if !send_error(
                        output,
                        None,
                        None,
                        AlignmentSceneEngineOperation::EvictInactive,
                        error,
                    ) {
                        return;
                    }
                }
            }
        } else if coordinator.has_pending_source_work() {
            let pump_started = Instant::now();
            let pump_result = coordinator.pump_once();
            if !report_slow_worker_phase(
                output,
                AlignmentSceneWorkerPhase::PumpSource,
                None,
                pump_started,
            ) {
                return;
            }
            match pump_result {
                Ok(pump) => {
                    for publication in pump.publications {
                        let Ok(Some(frame)) = coordinator.frame(publication.owner) else {
                            continue;
                        };
                        let Ok(telemetry) = coordinator.telemetry(publication.owner) else {
                            continue;
                        };
                        let Ok(work) = coordinator.owner_work_state(publication.owner) else {
                            continue;
                        };
                        if output
                            .send_latest_for(
                                publication.owner.get(),
                                AlignmentSceneEngineEvent::FrameUpdated {
                                    owner: publication.owner,
                                    request_token: current_request_token(
                                        &request_tokens,
                                        publication.owner,
                                    ),
                                    cause: AlignmentSceneFrameCause::SourcePublication,
                                    update: None,
                                    published_scene: Some(publication.revision),
                                    frame,
                                    work,
                                    telemetry,
                                },
                            )
                            .is_err()
                        {
                            return;
                        }
                    }
                    for refusal in pump.refusals {
                        let Ok(Some(frame)) = coordinator.frame(refusal.owner) else {
                            continue;
                        };
                        let Ok(telemetry) = coordinator.telemetry(refusal.owner) else {
                            continue;
                        };
                        // This is a terminal source decision, not replaceable viewport data. A
                        // later camera/clip frame may carry the same typed refusal, but must not
                        // erase the only explicit terminal edge before the desktop consumes it.
                        if output
                            .send_control(AlignmentSceneEngineEvent::IndividualReadsRefused {
                                owner: refusal.owner,
                                request_token: current_request_token(
                                    &request_tokens,
                                    refusal.owner,
                                ),
                                target: refusal.target,
                                complete_read_limit: refusal.complete_read_limit,
                                eligible_records: refusal.eligible_records,
                                frame,
                                telemetry,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    for pressure in pump.pressures {
                        schedule_internal_reclaim(
                            &mut coordinator,
                            &mut pending_eviction,
                            pressure.owner,
                            pressure.pressure,
                        );
                        if !send_resource_pressure(
                            output,
                            pressure.owner,
                            current_request_token(&request_tokens, pressure.owner),
                            pressure.target,
                            AlignmentSceneEngineOperation::PumpSource,
                            pressure.intent,
                            pressure.pressure,
                        ) {
                            return;
                        }
                    }
                }
                Err(error) => {
                    if !handle_source_pump_error(
                        &mut coordinator,
                        &request_tokens,
                        &mut pending_eviction,
                        output,
                        error,
                    ) {
                        return;
                    }
                }
            }
        } else if coordinator.has_pending_presentation_work() {
            let pump_started = Instant::now();
            let pump_result = coordinator.pump_presentation_once();
            if !report_slow_worker_phase(
                output,
                AlignmentSceneWorkerPhase::PumpPresentation,
                None,
                pump_started,
            ) {
                return;
            }
            match pump_result {
                Ok(Some(pump)) => {
                    if let Some(pressure) = pump.pressure {
                        if pump.intent == SourceIntentKind::Foreground {
                            schedule_internal_reclaim(
                                &mut coordinator,
                                &mut pending_eviction,
                                pump.owner,
                                pressure,
                            );
                        }
                        let target = coordinator
                            .frame(pump.owner)
                            .ok()
                            .flatten()
                            .map(|frame| frame.camera.interval());
                        let Some(target) = target else {
                            if !send_error(
                                output,
                                Some(pump.owner),
                                Some(current_request_token(&request_tokens, pump.owner)),
                                AlignmentSceneEngineOperation::PumpPresentation,
                                AlignmentSceneCoordinatorConfigError::UnknownOwner(pump.owner),
                            ) {
                                return;
                            }
                            continue;
                        };
                        if pump.intent == SourceIntentKind::Speculative
                            && pressure.intrinsically_exceeds_limit()
                        {
                            // Full-height residency is optional. The coordinator has already
                            // latched this lane as BudgetLimited while preserving its current
                            // visible-complete snapshot, so publish that truthful state instead of
                            // turning an oversized background row block into a read-data failure.
                            if !send_frame(
                                &coordinator,
                                output,
                                pump.owner,
                                current_request_token(&request_tokens, pump.owner),
                                AlignmentSceneFrameCause::VerticalResidency,
                                None,
                                None,
                            ) {
                                return;
                            }
                        } else if !send_resource_pressure(
                            output,
                            pump.owner,
                            current_request_token(&request_tokens, pump.owner),
                            target,
                            AlignmentSceneEngineOperation::PumpPresentation,
                            pump.intent,
                            pressure,
                        ) {
                            return;
                        }
                    } else if pump.published_scene.is_some() || pump.completed || pump.failed {
                        // Intermediate independent row-block warms neither replace the semantic
                        // scene nor require a desktop redraw. Publish one coalesced status/frame
                        // update only when the whole directory becomes warm or optional warming
                        // becomes unavailable. The latter preserves the complete visible scene.
                        if !send_frame(
                            &coordinator,
                            output,
                            pump.owner,
                            current_request_token(&request_tokens, pump.owner),
                            if pump.intent == SourceIntentKind::Foreground {
                                AlignmentSceneFrameCause::RowCamera
                            } else {
                                AlignmentSceneFrameCause::VerticalResidency
                            },
                            None,
                            pump.published_scene,
                        ) {
                            return;
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    if output
                        .send_control(AlignmentSceneEngineEvent::Error {
                            owner: None,
                            request_token: None,
                            operation: AlignmentSceneEngineOperation::PumpPresentation,
                            message: Arc::from(error.to_string()),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}

/// Converts a retained-budget refusal into the demand-derived low-water target maintained by the
/// source coordinator. The refused allocation expands the measured island envelope; it is never
/// used as an exact-fit reclaim threshold.
fn schedule_internal_reclaim<B>(
    coordinator: &mut AlignmentSceneSourceCoordinator<B>,
    pending_eviction: &mut Option<PendingInactiveEviction>,
    owner: SourceOwnerId,
    pressure: LedgerError,
) where
    B: CanonicalAlignmentSourceBackend,
{
    let LedgerError::BudgetExceeded { requested, .. } = pressure else {
        return;
    };
    let Ok(plan) = coordinator.foreground_capacity_plan(owner, requested) else {
        return;
    };
    if plan.charged <= plan.low_water_target {
        return;
    }
    schedule_reclaim_target(pending_eviction, 0, plan.low_water_target);
}

fn schedule_reclaim_target(
    pending_eviction: &mut Option<PendingInactiveEviction>,
    request_id: u64,
    target_charged: usize,
) {
    if let Some(pending) = pending_eviction {
        pending.target_charged = pending.target_charged.min(target_charged);
        // Preserve a desktop request ID so its cross-owner transaction receives completion even
        // when an internal foreground target tightens the same coalesced reclaim cycle.
        if pending.request_id == 0 && request_id != 0 {
            pending.request_id = request_id;
        }
        return;
    }
    *pending_eviction = Some(PendingInactiveEviction {
        request_id,
        target_charged,
        accumulated: None,
    });
}

fn camera_change_retargets_reclaim(
    previous: Option<AlignmentSceneCamera>,
    next: AlignmentSceneCamera,
) -> bool {
    previous.is_none_or(|previous| !previous.interval().overlaps(next.interval()))
}

/// Handles a failed bounded source checkpoint without widening an intent-local resource refusal
/// into a source-wide outage. The actor has already abandoned and cancelled the half-consumed
/// decoder transaction before this function runs.
fn handle_source_pump_error<B>(
    coordinator: &mut AlignmentSceneSourceCoordinator<B>,
    request_tokens: &BTreeMap<SourceOwnerId, AlignmentSceneRequestToken>,
    pending_eviction: &mut Option<PendingInactiveEviction>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    error: crate::AlignmentSceneCoordinatorError<B::Error>,
) -> bool
where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + Send + Sync + 'static,
{
    let failed_work = match &error {
        crate::AlignmentSceneCoordinatorError::Source(source) => source.work().cloned(),
        _ => None,
    };
    let pressure = source_allocation_pressure::<B>(&error);
    if let (Some(work), Some(pressure)) = (failed_work, pressure) {
        // A newer desired sequence may have replaced this work before the checkpoint failed. In
        // that case the failure is stale and must neither cancel nor mark the newer request.
        if !coordinator.actor_mut().cancel_failed_work_if_current(&work) {
            return true;
        }
        schedule_internal_reclaim(
            coordinator,
            pending_eviction,
            work.intent().owner(),
            pressure,
        );
        return send_resource_pressure(
            output,
            work.intent().owner(),
            current_request_token(request_tokens, work.intent().owner()),
            work.intent().spec().target(),
            AlignmentSceneEngineOperation::PumpSource,
            work.kind(),
            pressure,
        );
    }

    // Failures without a ledger-pressure cause can indicate a corrupt or unavailable shared
    // source. Preserve the existing source-wide terminal behavior for those cases.
    coordinator.cancel_pending_source_work();
    output
        .send_control(AlignmentSceneEngineEvent::Error {
            owner: None,
            request_token: None,
            operation: AlignmentSceneEngineOperation::PumpSource,
            message: Arc::from(error.to_string()),
        })
        .is_ok()
}

fn send_resource_pressure(
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    owner: SourceOwnerId,
    request_token: AlignmentSceneRequestToken,
    target: rgv_core::GenomicInterval,
    operation: AlignmentSceneEngineOperation,
    intent: SourceIntentKind,
    pressure: LedgerError,
) -> bool {
    send_control(
        output,
        AlignmentSceneEngineEvent::ResourcePressure {
            owner,
            request_token,
            target,
            operation,
            intent,
            pressure,
            disposition: AlignmentSceneResourcePressureDisposition::for_pressure(pressure),
        },
    )
}

fn allocation_pressure(error: &(dyn Error + 'static)) -> Option<LedgerError> {
    let mut current = Some(error);
    while let Some(candidate) = current {
        if let Some(pressure) = candidate.downcast_ref::<LedgerError>()
            && matches!(
                pressure,
                LedgerError::BudgetExceeded { .. }
                    | LedgerError::TransientBudgetExceeded { .. }
                    | LedgerError::ReplacementTransitionActive { .. }
            )
        {
            return Some(*pressure);
        }
        current = candidate.source();
    }
    None
}

fn source_allocation_pressure<B>(
    error: &crate::AlignmentSceneCoordinatorError<B::Error>,
) -> Option<LedgerError>
where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + 'static,
{
    match error {
        crate::AlignmentSceneCoordinatorError::Source(source) => source
            .backend_error()
            .and_then(B::allocation_pressure)
            .or_else(|| coordinator_budget_pressure(error))
            .or_else(|| allocation_pressure(error)),
        _ => coordinator_budget_pressure(error).or_else(|| allocation_pressure(error)),
    }
}

fn owner_pressure_target<B>(
    coordinator: &AlignmentSceneSourceCoordinator<B>,
    owner: SourceOwnerId,
) -> Option<rgv_core::GenomicInterval>
where
    B: CanonicalAlignmentSourceBackend,
{
    coordinator
        .frame(owner)
        .ok()
        .flatten()
        .map(|frame| frame.camera.interval())
}

/// Converts owner-local shared-ledger contention into a typed scheduling edge. Configuration,
/// identity, and backend failures remain ordinary terminal errors for the exact operation.
fn send_owner_operation_error<B>(
    coordinator: &mut AlignmentSceneSourceCoordinator<B>,
    pending_eviction: &mut Option<PendingInactiveEviction>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    owner: SourceOwnerId,
    request_token: AlignmentSceneRequestToken,
    operation: AlignmentSceneEngineOperation,
    intent: SourceIntentKind,
    error: crate::AlignmentSceneCoordinatorError<B::Error>,
) -> bool
where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + 'static,
{
    if let (Some(pressure), Some(target)) = (
        source_allocation_pressure::<B>(&error),
        owner_pressure_target(coordinator, owner),
    ) {
        if intent == SourceIntentKind::Foreground {
            schedule_internal_reclaim(coordinator, pending_eviction, owner, pressure);
        }
        // The desired lane state is already durable. Stop any older source authority from being
        // pumped under that newer token while capacity is unavailable; the explicit retry will
        // reconcile and resume only the still-current desired target.
        match intent {
            SourceIntentKind::Foreground => {
                coordinator.actor_mut().cancel_foreground_for(owner);
                coordinator.actor_mut().cancel_speculation_for(owner);
            }
            SourceIntentKind::Speculative => {
                coordinator.actor_mut().cancel_speculation_for(owner);
            }
        }
        return send_resource_pressure(
            output,
            owner,
            request_token,
            target,
            operation,
            intent,
            pressure,
        );
    }
    send_error(output, Some(owner), Some(request_token), operation, error)
}

/// Budget policy edits have one ordinary transient failure mode: `set_budget` cannot change the
/// policy while a prepared replacement is between commit and retirement settlement. Keep that
/// collision typed and nonterminal. Every other coordinator/ledger failure remains an ordinary
/// error; in particular, this must not turn bookkeeping corruption into a retry loop.
fn budget_reduction_pressure<E>(
    error: &crate::AlignmentSceneCoordinatorError<E>,
) -> Option<LedgerError> {
    match coordinator_budget_pressure(error) {
        Some(pressure @ LedgerError::ReplacementTransitionActive { .. }) => Some(pressure),
        Some(
            LedgerError::BudgetExceeded { .. }
            | LedgerError::TransientBudgetExceeded { .. }
            | LedgerError::ReservationExceeded { .. }
            | LedgerError::UnknownAllocation(_)
            | LedgerError::IdOverflow
            | LedgerError::Poisoned,
        )
        | None => None,
    }
}

fn send_budget_reduction_pressure_or_error<B>(
    coordinator: &AlignmentSceneSourceCoordinator<B>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    requested_budget: usize,
    clear_before_apply: bool,
    error: crate::AlignmentSceneCoordinatorError<B::Error>,
) -> bool
where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + 'static,
{
    if let Some(pressure) = budget_reduction_pressure(&error) {
        match coordinator.actor().store().ledger().progress() {
            Ok(progress) => send_control(
                output,
                AlignmentSceneEngineEvent::BudgetPressure {
                    requested_budget,
                    clear_before_apply,
                    current_policy_revision: progress.policy_revision,
                    current_budget: progress.budget,
                    pressure,
                },
            ),
            Err(progress_error) => send_error(
                output,
                None,
                None,
                AlignmentSceneEngineOperation::ReduceBudget,
                progress_error,
            ),
        }
    } else {
        send_error(
            output,
            None,
            None,
            AlignmentSceneEngineOperation::ReduceBudget,
            error,
        )
    }
}

fn apply_request<B>(
    coordinator: &mut AlignmentSceneSourceCoordinator<B>,
    request_tokens: &mut BTreeMap<SourceOwnerId, AlignmentSceneRequestToken>,
    pending_eviction: &mut Option<PendingInactiveEviction>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    request: AlignmentSceneEngineRequest<B::Control>,
) -> bool
where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + Send + Sync + 'static,
{
    match request {
        AlignmentSceneEngineRequest::InsertLane {
            owner,
            request_token,
            contract,
            policy,
        } => {
            request_tokens.insert(owner, request_token);
            match coordinator.insert_lane(owner, contract, policy) {
                Ok(()) => send_control(output, AlignmentSceneEngineEvent::LaneInserted { owner }),
                Err(error) => send_error(
                    output,
                    Some(owner),
                    Some(request_token),
                    AlignmentSceneEngineOperation::InsertLane,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::RemoveLane { owner } => {
            let existed = coordinator.remove_lane(owner);
            request_tokens.remove(&owner);
            send_control(
                output,
                AlignmentSceneEngineEvent::LaneRemoved { owner, existed },
            )
        }
        AlignmentSceneEngineRequest::UpdateCamera {
            owner,
            request_token,
            camera,
        } => {
            let previous_camera = coordinator
                .frame(owner)
                .ok()
                .flatten()
                .map(|frame| frame.camera);
            if camera_change_retargets_reclaim(previous_camera, camera) {
                // Only a genuinely disjoint navigation island invalidates the old reserve.
                // Contained pans, zooms, and row motion keep making low-water progress instead of
                // cancelling maintenance on every pointer sample.
                if !supersede_pending_eviction(
                    pending_eviction,
                    output,
                    AlignmentInactiveEvictionSupersession::DisjointCamera,
                ) {
                    return false;
                }
            }
            request_tokens.insert(owner, request_token);
            let update_started = Instant::now();
            let update_result = coordinator.update_camera(owner, camera);
            if !report_slow_worker_phase(
                output,
                AlignmentSceneWorkerPhase::UpdateCamera,
                Some(owner),
                update_started,
            ) {
                return false;
            }
            match update_result {
                Ok(update) => {
                    if let Some(target) = update.proactive_reclaim_target {
                        schedule_reclaim_target(pending_eviction, 0, target);
                    }
                    send_frame(
                        coordinator,
                        output,
                        owner,
                        request_token,
                        AlignmentSceneFrameCause::Camera,
                        Some(update),
                        update.published_scene,
                    )
                }
                Err(error) => send_owner_operation_error(
                    coordinator,
                    pending_eviction,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneEngineOperation::UpdateCamera,
                    SourceIntentKind::Foreground,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::SetOptionalWorkPaused { owner, paused } => coordinator
            .set_optional_work_paused(owner, paused)
            .map_or_else(
                |error| {
                    send_error(
                        output,
                        Some(owner),
                        Some(current_request_token(request_tokens, owner)),
                        AlignmentSceneEngineOperation::PumpPresentation,
                        error,
                    )
                },
                |()| true,
            ),
        AlignmentSceneEngineRequest::ReplaceContract {
            owner,
            request_token,
            contract,
        } => {
            request_tokens.insert(owner, request_token);
            match coordinator.replace_contract(owner, contract) {
                Ok(update) => send_frame(
                    coordinator,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneFrameCause::Contract,
                    Some(update),
                    update.published_scene,
                ),
                Err(error) => send_owner_operation_error(
                    coordinator,
                    pending_eviction,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneEngineOperation::ReplaceContract,
                    SourceIntentKind::Foreground,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::InstallReferenceAuthority { owner, authority } => {
            let request_token = current_request_token(request_tokens, owner);
            match coordinator.install_reference_authority(owner, authority) {
                Ok(outcome) if outcome.installed => send_frame(
                    coordinator,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneFrameCause::ReferenceAuthority,
                    None,
                    outcome.published_scene,
                ),
                Ok(_) => true,
                Err(error) => send_owner_operation_error(
                    coordinator,
                    pending_eviction,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneEngineOperation::InstallReferenceAuthority,
                    SourceIntentKind::Foreground,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::SetPreparationPolicy {
            owner,
            request_token,
            policy,
        } => {
            request_tokens.insert(owner, request_token);
            match coordinator.set_preparation_policy(owner, policy) {
                Ok(Some(update)) => send_frame(
                    coordinator,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneFrameCause::PreparationPolicy,
                    Some(update),
                    update.published_scene,
                ),
                Ok(None) => true,
                Err(error) => send_owner_operation_error(
                    coordinator,
                    pending_eviction,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneEngineOperation::SetPreparationPolicy,
                    SourceIntentKind::Foreground,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::PrewarmPresentation {
            owner,
            presentation,
        } => match coordinator.prewarm_presentation(owner, presentation) {
            Ok(prepared) => send_control(
                output,
                AlignmentSceneEngineEvent::PresentationPrewarmed { owner, prepared },
            ),
            Err(error) => send_owner_operation_error(
                coordinator,
                pending_eviction,
                output,
                owner,
                current_request_token(request_tokens, owner),
                AlignmentSceneEngineOperation::PrewarmPresentation,
                SourceIntentKind::Speculative,
                error,
            ),
        },
        AlignmentSceneEngineRequest::SetSourceControl(control) => {
            let retry_refusals = B::control_invalidates_read_refusals(&control);
            match coordinator.actor_mut().submit_control(control) {
                Ok(()) => {
                    if !send_control(output, AlignmentSceneEngineEvent::SourceControlQueued) {
                        return false;
                    }
                    if !retry_refusals {
                        return true;
                    }
                    match coordinator.retry_refused_lanes() {
                        Ok(retries) => retries.into_iter().all(|retry| {
                            send_frame(
                                coordinator,
                                output,
                                retry.owner,
                                current_request_token(request_tokens, retry.owner),
                                AlignmentSceneFrameCause::SourceControlRetry,
                                None,
                                None,
                            )
                        }),
                        Err(error) => send_error(
                            output,
                            None,
                            None,
                            AlignmentSceneEngineOperation::SetSourceControl,
                            error,
                        ),
                    }
                }
                Err(error) => send_error(
                    output,
                    None,
                    None,
                    AlignmentSceneEngineOperation::SetSourceControl,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::ReduceBudget { budget } => {
            if !supersede_pending_eviction(
                pending_eviction,
                output,
                AlignmentInactiveEvictionSupersession::BudgetPolicyChange,
            ) {
                return false;
            }
            match coordinator.reduce_budget(budget) {
                Ok(pressure) => send_control(
                    output,
                    AlignmentSceneEngineEvent::BudgetReduced { pressure },
                ),
                Err(error) => send_budget_reduction_pressure_or_error(
                    coordinator,
                    output,
                    budget,
                    false,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::EvictInactive {
            request_id,
            target_charged,
        } => match coordinator.aggregate_foreground_low_water_target() {
            Ok(measured_target) => {
                schedule_reclaim_target(
                    pending_eviction,
                    request_id,
                    measured_target.map_or(target_charged, |measured| measured.min(target_charged)),
                );
                true
            }
            Err(error) => send_error(
                output,
                None,
                None,
                AlignmentSceneEngineOperation::EvictInactive,
                error,
            ),
        },
        AlignmentSceneEngineRequest::ClearCacheBudget { restore_budget } => {
            if !supersede_pending_eviction(
                pending_eviction,
                output,
                AlignmentInactiveEvictionSupersession::CacheClear,
            ) {
                return false;
            }
            match coordinator.reduce_budget(0) {
                Ok(_) => match coordinator.reduce_budget(restore_budget) {
                    Ok(pressure) => send_control(
                        output,
                        AlignmentSceneEngineEvent::BudgetReduced { pressure },
                    ),
                    Err(error) => send_budget_reduction_pressure_or_error(
                        coordinator,
                        output,
                        restore_budget,
                        false,
                        error,
                    ),
                },
                Err(error) => send_budget_reduction_pressure_or_error(
                    coordinator,
                    output,
                    restore_budget,
                    true,
                    error,
                ),
            }
        }
        AlignmentSceneEngineRequest::RetryResourcePressure {
            owner,
            request_token,
            target,
        } => {
            // The UI can consume a pressure event after a newer camera has already entered the
            // mailbox. Never let that stale relief signal reconstruct or publish old work.
            if request_tokens.get(&owner).copied() != Some(request_token) {
                return true;
            }
            match coordinator.retry_resource_pressure(owner, target) {
                Ok(Some(update)) => send_frame(
                    coordinator,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneFrameCause::ResourcePressureRetry,
                    Some(update),
                    update.published_scene,
                ),
                Ok(None) => true,
                Err(error) => {
                    if let Some(pressure) = source_allocation_pressure::<B>(&error) {
                        send_resource_pressure(
                            output,
                            owner,
                            request_token,
                            target,
                            AlignmentSceneEngineOperation::RetryResourcePressure,
                            SourceIntentKind::Foreground,
                            pressure,
                        )
                    } else {
                        send_error(
                            output,
                            Some(owner),
                            Some(request_token),
                            AlignmentSceneEngineOperation::RetryResourcePressure,
                            error,
                        )
                    }
                }
            }
        }
    }
}

fn send_frame<B>(
    coordinator: &AlignmentSceneSourceCoordinator<B>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    owner: SourceOwnerId,
    request_token: AlignmentSceneRequestToken,
    cause: AlignmentSceneFrameCause,
    update: Option<AlignmentCoordinatorUpdate>,
    published_scene: Option<SceneRevision>,
) -> bool
where
    B: CanonicalAlignmentSourceBackend,
{
    let started = Instant::now();
    let Ok(Some(frame)) = coordinator.frame(owner) else {
        return send_error(
            output,
            Some(owner),
            Some(request_token),
            AlignmentSceneEngineOperation::UpdateCamera,
            AlignmentSceneCoordinatorConfigError::UnknownOwner(owner),
        );
    };
    let Ok(telemetry) = coordinator.telemetry(owner) else {
        return send_error(
            output,
            Some(owner),
            Some(request_token),
            AlignmentSceneEngineOperation::UpdateCamera,
            AlignmentSceneCoordinatorConfigError::UnknownOwner(owner),
        );
    };
    let Ok(work) = coordinator.owner_work_state(owner) else {
        return send_error(
            output,
            Some(owner),
            Some(request_token),
            AlignmentSceneEngineOperation::UpdateCamera,
            AlignmentSceneCoordinatorConfigError::UnknownOwner(owner),
        );
    };
    let sent = output
        .send_latest_for(
            owner.get(),
            AlignmentSceneEngineEvent::FrameUpdated {
                owner,
                request_token,
                cause,
                update,
                published_scene,
                frame,
                work,
                telemetry,
            },
        )
        .is_ok();
    sent && report_slow_worker_phase(
        output,
        AlignmentSceneWorkerPhase::PublishFrame,
        Some(owner),
        started,
    )
}

fn send_control(
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    event: AlignmentSceneEngineEvent,
) -> bool {
    output.send_control(event).is_ok()
}

fn send_error(
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
    owner: Option<SourceOwnerId>,
    request_token: Option<AlignmentSceneRequestToken>,
    operation: AlignmentSceneEngineOperation,
    error: impl std::fmt::Display,
) -> bool {
    send_control(
        output,
        AlignmentSceneEngineEvent::Error {
            owner,
            request_token,
            operation,
            message: Arc::from(error.to_string()),
        },
    )
}

fn current_request_token(
    request_tokens: &BTreeMap<SourceOwnerId, AlignmentSceneRequestToken>,
    owner: SourceOwnerId,
) -> AlignmentSceneRequestToken {
    request_tokens.get(&owner).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroU64, NonZeroUsize},
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread::ThreadId,
        time::Instant,
    };

    use rgv_alignment_scene::{
        AlignmentCamera, AlignmentStyleContract, AlleleSummaryDetail, AllocationBundleActual,
        AllocationBundleDemand, AllocationBundleSlot, AllocationBundleSpec, AllocationClass,
        AllocationLedger, AllocationRetirementClaimSet, AllocationRetirementToken,
        CapabilityRequirement, EvidenceStore, GeometryLod, LayoutContract, PayloadContract,
        PresentationContract, RevisionFingerprint, SourceFingerprint, SummaryContract,
    };
    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId, GenomicInterval,
        OverlappingMatePolicy, ReadStackingMode,
    };
    use rgv_hts::{
        AlignmentOpenOptions, AlignmentSource, LocalAlignmentSource, QueryOptions,
        TemplateLengthBounds,
    };

    use super::*;
    use crate::alignment_scene_coordinator::AlignmentVerticalResidencyPumpOutcome;
    use crate::{
        AlignmentSceneCoordinatorError, AlignmentSourceActor, AlignmentSourceActorError,
        AlignmentSourceIntentSpec, CanonicalDecodeCheckpoint, LocalBamCanonicalBackend,
        LocalBamCanonicalBackendError, LocalBamSourceControl, SourceWork,
    };

    #[test]
    fn row_camera_changes_preserve_a_valid_reclaim_transaction() {
        let contig = ContigId::new(0);
        let render_camera = AlignmentCamera::new(
            GenomicInterval::new(contig, 100, 200).unwrap(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 72.0,
            },
        )
        .unwrap();
        let current: AlignmentSceneCamera = render_camera.into();
        let scrolled = AlignmentSceneCamera::with_visible_rows(current.viewport(), 10..20).unwrap();
        assert!(!camera_change_retargets_reclaim(Some(current), scrolled));

        let panned: AlignmentSceneCamera = AlignmentCamera::new(
            GenomicInterval::new(contig, 101, 201).unwrap(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 72.0,
            },
        )
        .unwrap()
        .into();
        assert!(!camera_change_retargets_reclaim(Some(current), panned));
        let disjoint: AlignmentSceneCamera = AlignmentCamera::new(
            GenomicInterval::new(contig, 500, 600).unwrap(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 72.0,
            },
        )
        .unwrap()
        .into();
        assert!(camera_change_retargets_reclaim(Some(current), disjoint));
        assert!(camera_change_retargets_reclaim(None, current));
    }

    #[test]
    fn reclaim_requests_coalesce_to_the_strictest_low_water_without_losing_completion_identity() {
        let mut pending = None;
        schedule_reclaim_target(&mut pending, 0, 900);
        schedule_reclaim_target(&mut pending, 41, 800);
        schedule_reclaim_target(&mut pending, 0, 850);

        let pending = pending.expect("coalesced reclaim remains scheduled");
        assert_eq!(pending.request_id, 41);
        assert_eq!(pending.target_charged, 800);
        assert!(pending.accumulated.is_none());
    }

    #[test]
    fn reclaim_supersession_is_an_explicit_terminal_event() {
        let (output, input) = bounded_coalescing(4);
        let mut pending = Some(PendingInactiveEviction {
            request_id: 73,
            target_charged: 800,
            accumulated: None,
        });

        assert!(supersede_pending_eviction(
            &mut pending,
            &output,
            AlignmentInactiveEvictionSupersession::DisjointCamera,
        ));
        assert!(pending.is_none());
        let event = input
            .try_receive()
            .unwrap()
            .expect("supersession emits one terminal event");
        assert!(matches!(
            event,
            AlignmentSceneEngineEvent::InactiveEvictionSuperseded {
                request_id: 73,
                target_charged: 800,
                released_bytes: 0,
                relinquished_cache_entries: 0,
                reason: AlignmentInactiveEvictionSupersession::DisjointCamera,
            }
        ));
    }

    #[test]
    fn worker_error_message_preserves_the_concrete_backend_cause() {
        let error: AlignmentSceneCoordinatorError<LocalBamCanonicalBackendError> =
            AlignmentSourceActorError::Backend(
                LocalBamCanonicalBackendError::UnexpectedDecoderCancellation,
            )
            .into();

        assert_eq!(
            error.to_string(),
            "alignment source backend failed: the decoder cancelled despite the actor retaining active work"
        );
    }

    struct LedgerPressureBackend {
        checkpoints: Arc<AtomicUsize>,
    }

    struct EmptyRetirementToken(AllocationRetirementClaimSet);

    impl AllocationRetirementToken for EmptyRetirementToken {
        fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
            &self.0
        }
    }

    impl CanonicalAlignmentSourceBackend for LedgerPressureBackend {
        type Control = ();
        type Error = LocalBamCanonicalBackendError;

        fn allocation_pressure(error: &Self::Error) -> Option<LedgerError> {
            LocalBamCanonicalBackend::allocation_pressure(error)
        }

        fn apply_control(&mut self, _control: Self::Control) -> Result<(), Self::Error> {
            Ok(())
        }

        fn begin_work(
            &mut self,
            _work: &SourceWork,
            _ledger: &AllocationLedger,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn next_checkpoint(
            &mut self,
            _work: &SourceWork,
            _ledger: &AllocationLedger,
        ) -> Result<CanonicalDecodeCheckpoint, Self::Error> {
            self.checkpoints.fetch_add(1, Ordering::SeqCst);
            Err(LocalBamCanonicalBackendError::Ledger(
                LedgerError::BudgetExceeded {
                    requested: 64,
                    charged: 960,
                    reserved: 0,
                    limit: 1_000,
                },
            ))
        }

        fn retarget(&mut self, _work: &SourceWork) -> Result<(), Self::Error> {
            Ok(())
        }

        fn cancel(&mut self, _work: &SourceWork) {}
    }

    fn pressure_engine(
        owner: SourceOwnerId,
        kind: SourceIntentKind,
    ) -> (
        AlignmentSceneEngine<LedgerPressureBackend>,
        Arc<AtomicUsize>,
    ) {
        let source_revision = AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        );
        let record_contract = ContractFingerprint::new(9);
        let ledger = AllocationLedger::new(1_000, 0);
        let store = EvidenceStore::new(source_revision, record_contract, ledger);
        let checkpoints = Arc::new(AtomicUsize::new(0));
        let backend = LedgerPressureBackend {
            checkpoints: Arc::clone(&checkpoints),
        };
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let spec = AlignmentSourceIntentSpec::new(
            source_revision,
            record_contract,
            PayloadContract::new(
                CapabilityRequirement::NotRequired,
                CapabilityRequirement::NotRequired,
                [],
                None,
            ),
            GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        );
        match kind {
            SourceIntentKind::Foreground => {
                actor.submit_foreground_for(owner, spec).unwrap();
            }
            SourceIntentKind::Speculative => {
                actor.submit_speculation_for(owner, spec).unwrap();
            }
        }
        (
            AlignmentSceneEngine::spawn(AlignmentSceneSourceCoordinator::new(actor), 8),
            checkpoints,
        )
    }

    #[test]
    fn foreground_allocation_pressure_is_typed_owner_scoped_and_nonterminal() {
        let owner = SourceOwnerId::new(301);
        let (engine, checkpoints) = pressure_engine(owner, SourceIntentKind::Foreground);
        let event = receive_matching(&engine, |event| {
            matches!(event, AlignmentSceneEngineEvent::ResourcePressure { .. })
        });
        let AlignmentSceneEngineEvent::ResourcePressure {
            owner: seen,
            target,
            operation,
            intent,
            pressure,
            disposition,
            ..
        } = event
        else {
            unreachable!("the matching event is typed resource pressure")
        };
        assert_eq!(seen, owner);
        assert_eq!(operation, AlignmentSceneEngineOperation::PumpSource);
        assert_eq!(
            target,
            GenomicInterval::new(ContigId::new(0), 100, 200).unwrap()
        );
        assert_eq!(intent, SourceIntentKind::Foreground);
        assert_eq!(
            pressure,
            LedgerError::BudgetExceeded {
                requested: 64,
                charged: 960,
                reserved: 0,
                limit: 1_000,
            }
        );
        assert_eq!(
            disposition,
            AlignmentSceneResourcePressureDisposition::Retryable
        );
        assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "an owner-local budget refusal must not restart in a busy loop"
        );
    }

    #[test]
    fn speculative_allocation_pressure_is_typed_and_pauses_without_busy_loop() {
        let owner = SourceOwnerId::new(302);
        let (engine, checkpoints) = pressure_engine(owner, SourceIntentKind::Speculative);
        let event = receive_matching(&engine, |event| {
            matches!(event, AlignmentSceneEngineEvent::ResourcePressure { .. })
        });
        let AlignmentSceneEngineEvent::ResourcePressure {
            owner: seen,
            operation,
            intent,
            disposition,
            ..
        } = event
        else {
            unreachable!("the matching event is typed resource pressure")
        };
        assert_eq!(seen, owner);
        assert_eq!(operation, AlignmentSceneEngineOperation::PumpSource);
        assert_eq!(intent, SourceIntentKind::Speculative);
        assert_eq!(
            disposition,
            AlignmentSceneResourcePressureDisposition::Retryable
        );
        assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "speculative pressure must pause rather than restart in a busy loop"
        );
    }

    #[test]
    fn pressure_disposition_distinguishes_occupancy_from_intrinsically_oversized_work() {
        assert_eq!(
            AlignmentSceneResourcePressureDisposition::for_pressure(
                LedgerError::ReplacementTransitionActive { bundle_id: 7 }
            ),
            AlignmentSceneResourcePressureDisposition::Retryable
        );
        assert_eq!(
            AlignmentSceneResourcePressureDisposition::for_pressure(LedgerError::BudgetExceeded {
                requested: 100,
                charged: 950,
                reserved: 0,
                limit: 1_000,
            }),
            AlignmentSceneResourcePressureDisposition::Retryable
        );
        assert_eq!(
            AlignmentSceneResourcePressureDisposition::for_pressure(LedgerError::BudgetExceeded {
                requested: 1_001,
                charged: 0,
                reserved: 0,
                limit: 1_000,
            }),
            AlignmentSceneResourcePressureDisposition::Infeasible
        );
    }

    #[test]
    fn budget_reduction_only_defers_the_prepared_replacement_collision() {
        let replacement =
            crate::AlignmentSceneCoordinatorError::<LocalBamCanonicalBackendError>::Store(
                rgv_alignment_scene::EvidenceStoreError::Ledger(
                    LedgerError::ReplacementTransitionActive { bundle_id: 17 },
                ),
            );
        assert_eq!(
            budget_reduction_pressure(&replacement),
            Some(LedgerError::ReplacementTransitionActive { bundle_id: 17 })
        );

        let accounting =
            crate::AlignmentSceneCoordinatorError::<LocalBamCanonicalBackendError>::Store(
                rgv_alignment_scene::EvidenceStoreError::Ledger(LedgerError::BudgetExceeded {
                    requested: 8,
                    charged: 64,
                    reserved: 0,
                    limit: 64,
                }),
            );
        assert_eq!(budget_reduction_pressure(&accounting), None);

        let configuration =
            crate::AlignmentSceneCoordinatorError::<LocalBamCanonicalBackendError>::Config(
                AlignmentSceneCoordinatorConfigError::EmptyContig,
            );
        assert_eq!(budget_reduction_pressure(&configuration), None);
    }

    struct ThreadRecordingBackend {
        inner: LocalBamCanonicalBackend,
        threads: Arc<Mutex<Vec<ThreadId>>>,
    }

    impl ThreadRecordingBackend {
        fn record_thread(&self) {
            self.threads
                .lock()
                .expect("thread recorder mutex poisoned")
                .push(thread::current().id());
        }
    }

    impl CanonicalAlignmentSourceBackend for ThreadRecordingBackend {
        type Control = LocalBamSourceControl;
        type Error = LocalBamCanonicalBackendError;

        fn allocation_pressure(error: &Self::Error) -> Option<LedgerError> {
            LocalBamCanonicalBackend::allocation_pressure(error)
        }

        fn control_invalidates_read_refusals(control: &Self::Control) -> bool {
            LocalBamCanonicalBackend::control_invalidates_read_refusals(control)
        }

        fn apply_control(&mut self, control: Self::Control) -> Result<(), Self::Error> {
            self.record_thread();
            self.inner.apply_control(control)
        }

        fn begin_work(
            &mut self,
            work: &SourceWork,
            ledger: &AllocationLedger,
        ) -> Result<(), Self::Error> {
            self.record_thread();
            self.inner.begin_work(work, ledger)
        }

        fn next_checkpoint(
            &mut self,
            work: &SourceWork,
            ledger: &AllocationLedger,
        ) -> Result<CanonicalDecodeCheckpoint, Self::Error> {
            self.record_thread();
            self.inner.next_checkpoint(work, ledger)
        }

        fn retarget(&mut self, work: &SourceWork) -> Result<(), Self::Error> {
            self.record_thread();
            self.inner.retarget(work)
        }

        fn cancel(&mut self, work: &SourceWork) {
            self.record_thread();
            self.inner.cancel(work);
        }
    }

    fn test_engine() -> (
        AlignmentSceneEngine<ThreadRecordingBackend>,
        AlignmentSceneBuildContract,
        AlignmentScenePreparationPolicy,
        AlignmentCamera,
        Arc<Mutex<Vec<ThreadId>>>,
    ) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let contig = ContigId::new(0);
        let contig_length = source.reference_dictionary().get(contig).unwrap().length();
        let backend = LocalBamCanonicalBackend::new(
            source,
            QueryOptions {
                max_records: NonZeroUsize::new(1_000).unwrap(),
                ..QueryOptions::default()
            },
        )
        .unwrap();
        let source_revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let threads = Arc::new(Mutex::new(Vec::new()));
        let backend = ThreadRecordingBackend {
            inner: backend,
            threads: Arc::clone(&threads),
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 32 << 20, 64 << 20);
        let store = EvidenceStore::new(source_revision, record_contract, ledger);
        let actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let contract = AlignmentSceneBuildContract {
            source_revision,
            record_contract,
            contig,
            contig_length,
            payload: PayloadContract::new(
                CapabilityRequirement::NotRequired,
                CapabilityRequirement::NotRequired,
                [*b"XS"],
                None,
            ),
            view_filter: rgv_alignment_scene::AlignmentViewFilterContract::default(),
            highlight_filter: rgv_alignment_scene::AlignmentViewFilterContract::default(),
            summaries: SummaryContract {
                overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
                minimum_base_quality: 0,
                minimum_junction_flank: 0,
                allele_detail: AlleleSummaryDetail::None,
                collect_modifications: false,
            },
            layout: LayoutContract {
                stacking: ReadStackingMode::Compact,
                grouping: AlignmentGroupMode::None,
                group_order: AlignmentOrderDirection::Natural,
                sorting: AlignmentSortMode::Start,
                sort_order: AlignmentOrderDirection::Natural,
                sort_anchor: None,
                view_as_pairs: false,
                pair_length_bounds: TemplateLengthBounds::default(),
                promoted_template: None,
            },
            presentation: PresentationContract {
                geometry_lod: GeometryLod::Read,
                style: AlignmentStyleContract {
                    show_modified_bases: false,
                    ..AlignmentStyleContract::default()
                },
                reference: None,
                annotation: None,
            },
            geometry_tile_bases: NonZeroU64::new(1).unwrap(),
            summary_tile_bases: NonZeroU64::new(1).unwrap(),
            row_block_rows: NonZeroU32::new(8).unwrap(),
            normalization_mapped_records: None,
            reference: None,
            annotation: None,
        };
        let policy = AlignmentScenePreparationPolicy::new(1.0, 1.0, 0.25, 4, 1).unwrap();
        let camera = AlignmentCamera::new(
            GenomicInterval::new(contig, 9_999, 10_001).unwrap(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 72.0,
            },
        )
        .unwrap();
        (
            AlignmentSceneEngine::spawn(AlignmentSceneSourceCoordinator::new(actor), 8),
            contract,
            policy,
            camera,
            threads,
        )
    }

    #[test]
    fn worker_publishes_immutable_frames_without_decoding_on_the_caller_thread() {
        let caller = thread::current().id();
        let (engine, contract, policy, camera, threads) = test_engine();
        let owner = SourceOwnerId::new(42);
        engine.try_insert_lane(owner, contract, policy).unwrap();
        assert!(matches!(
            receive_matching(&engine, |event| matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )),
            AlignmentSceneEngineEvent::LaneInserted { .. }
        ));

        engine.try_update_camera(owner, camera).unwrap();
        let complete = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated { owner: seen, frame, .. }
                    if *seen == owner && frame.visible_complete()
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated {
            frame,
            published_scene,
            ..
        } = complete
        else {
            unreachable!("matching event has the asserted frame shape")
        };
        assert!(published_scene.is_some());
        assert!(frame.snapshot.is_some());
        assert!(engine.ledger().snapshot().unwrap().charged > 0);
        let observed = threads
            .lock()
            .expect("thread recorder mutex poisoned")
            .clone();
        assert!(!observed.is_empty());
        assert!(observed.into_iter().all(|worker| worker != caller));
    }

    #[test]
    fn eviction_only_control_preserves_the_configured_budget_and_correlates_its_answer() {
        let (engine, _, _, _, _) = test_engine();
        let before = engine.ledger().progress().unwrap();

        engine
            .try_evict_inactive_for_headroom(73, before.charged.saturating_sub(1))
            .unwrap();
        let event = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::InactiveEvicted { request_id: 73, .. }
            )
        });
        let AlignmentSceneEngineEvent::InactiveEvicted {
            request_id,
            eviction,
        } = event
        else {
            unreachable!()
        };
        assert_eq!(request_id, 73);
        assert_eq!(eviction.target_charged, before.charged.saturating_sub(1));
        assert_eq!(engine.ledger().progress().unwrap().budget, before.budget);
    }

    #[test]
    fn active_gpu_settlement_pressure_preserves_latest_camera_and_rejects_a_stale_retry() {
        let (engine, mut contract, policy, camera, _) = test_engine();
        let owner = SourceOwnerId::new(43);
        let initial_request = AlignmentSceneRequestToken::new(70);
        let pressured_request = AlignmentSceneRequestToken::new(71);
        let latest_request = AlignmentSceneRequestToken::new(72);
        engine
            .try_insert_lane(owner, contract.clone(), policy)
            .unwrap();
        let _ = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });
        engine
            .try_update_camera_for_request(owner, initial_request, camera)
            .unwrap();
        let _ = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_token,
                    frame,
                    ..
                } if *seen == owner
                    && *seen_token == initial_request
                    && frame.visible_complete()
            )
        });
        // Hold the ledger at the narrow prepared-settlement boundary which the user's
        // zoom-in/zoom-out race reached. The source roots are already resident. First leave a
        // changed presentation contract durably desired under pressure, then advance the camera:
        // that second collision is owned by UpdateCamera itself rather than BAM I/O.
        let ledger = engine.ledger().clone();
        let snapshot = ledger.snapshot().unwrap();
        let slot = AllocationBundleSlot::new(0);
        let claims = ledger.empty_retirement_claim_set();
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                snapshot.policy_revision,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::TransitionFrame,
                    1,
                )],
                claims.clone(),
            ))
            .unwrap();
        let commit = bundle
            .commit(vec![AllocationBundleActual::new(slot, 1, 1)])
            .unwrap();
        let mut retirement = Some(EmptyRetirementToken(claims));
        let prepared = commit
            .prepare_retirement_settlement(&mut retirement)
            .unwrap();

        contract.presentation.style.show_direction_terminals = false;
        engine
            .try_replace_contract_for_request(owner, pressured_request, contract)
            .unwrap();
        let pressure_event = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner: seen,
                    request_token: seen_token,
                    operation: AlignmentSceneEngineOperation::ReplaceContract,
                    pressure: LedgerError::ReplacementTransitionActive { .. },
                    ..
                } if *seen == owner && *seen_token == pressured_request
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    request_token: Some(seen_token),
                    ..
                } if *seen == owner && *seen_token == pressured_request
            )
        });
        let AlignmentSceneEngineEvent::ResourcePressure {
            target,
            operation,
            disposition,
            ..
        } = pressure_event
        else {
            panic!("contract setup pressure became a terminal error: {pressure_event:?}")
        };
        assert_eq!(operation, AlignmentSceneEngineOperation::ReplaceContract);
        assert_eq!(
            disposition,
            AlignmentSceneResourcePressureDisposition::Retryable
        );

        // A newer camera and its token must win even if a relief signal for the older pressure is
        // queued immediately behind it. The retry uses the ordered control lane specifically so it
        // cannot overwrite the unread latest-wins camera slot.
        let latest_camera = camera;
        engine
            .try_update_camera_for_request(owner, latest_request, latest_camera)
            .unwrap();
        engine
            .try_retry_resource_pressure(owner, pressured_request, target)
            .unwrap();
        let latest_pressure = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner: seen,
                    request_token: seen_token,
                    operation: AlignmentSceneEngineOperation::UpdateCamera,
                    pressure: LedgerError::ReplacementTransitionActive { .. },
                    ..
                } if *seen == owner && *seen_token == latest_request
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    request_token: Some(seen_token),
                    ..
                } if *seen == owner && *seen_token == latest_request
            )
        });
        let AlignmentSceneEngineEvent::ResourcePressure {
            target: latest_target,
            ..
        } = latest_pressure
        else {
            panic!("the newer UpdateCamera pressure became terminal: {latest_pressure:?}")
        };

        let settled = prepared.retire_and_settle();
        assert_eq!(settled.len(), 1);
        engine
            .try_retry_resource_pressure(owner, latest_request, latest_target)
            .unwrap();
        let resumed = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_token,
                    cause: AlignmentSceneFrameCause::ResourcePressureRetry,
                    published_scene: Some(_),
                    frame,
                    ..
                } if *seen == owner
                    && *seen_token == latest_request
                    && frame.camera == latest_camera
                    && frame.visible_complete()
                    && frame.snapshot.as_ref().is_some_and(|scene| {
                        !scene.reads().presentation().style.show_direction_terminals
                    })
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    request_token: Some(seen_token),
                    ..
                } if *seen == owner && *seen_token == latest_request
            )
        });
        assert!(
            matches!(resumed, AlignmentSceneEngineEvent::FrameUpdated { .. }),
            "the exact latest request must resume to a frame rather than becoming unavailable: {resumed:?}"
        );
        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "a stale retry or resumed exact request published more than once"
        );
    }

    #[test]
    fn active_gpu_settlement_reports_budget_pressure_without_faking_budget_reduction() {
        let (engine, _, _, _, _) = test_engine();
        let ledger = engine.ledger().clone();
        let before = ledger.snapshot().unwrap();
        let slot = AllocationBundleSlot::new(0);
        let claims = ledger.empty_retirement_claim_set();
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                before.policy_revision,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::TransitionFrame,
                    1,
                )],
                claims.clone(),
            ))
            .unwrap();
        let bundle_id = ledger
            .snapshot()
            .unwrap()
            .active_replacement_bundle
            .expect("the reserved replacement owns the ledger transition");
        let commit = bundle
            .commit(vec![AllocationBundleActual::new(slot, 1, 1)])
            .unwrap();
        let mut retirement = Some(EmptyRetirementToken(claims));
        let prepared = commit
            .prepare_retirement_settlement(&mut retirement)
            .unwrap();
        let requested = before.budget / 2;

        engine.try_reduce_budget(requested).unwrap();
        let event = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::BudgetPressure {
                    requested_budget,
                    ..
                } if *requested_budget == requested
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    operation: AlignmentSceneEngineOperation::ReduceBudget,
                    ..
                }
            )
        });
        let AlignmentSceneEngineEvent::BudgetPressure {
            requested_budget,
            clear_before_apply,
            current_policy_revision,
            current_budget,
            pressure,
        } = event
        else {
            panic!("prepared replacement budget collision became terminal: {event:?}")
        };
        assert_eq!(requested_budget, requested);
        assert!(!clear_before_apply);
        assert_eq!(current_policy_revision, before.policy_revision);
        assert_eq!(current_budget, before.budget);
        assert_eq!(
            pressure,
            LedgerError::ReplacementTransitionActive { bundle_id }
        );
        assert_eq!(ledger.snapshot().unwrap().budget, before.budget);
        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "a budget collision must wait for an explicit progress-gated retry"
        );

        let settled = prepared.retire_and_settle();
        assert_eq!(settled.len(), 1);
        engine.try_reduce_budget(requested).unwrap();
        let reduced = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::BudgetReduced { pressure }
                    if pressure.budget == requested
            )
        });
        assert!(matches!(
            reduced,
            AlignmentSceneEngineEvent::BudgetReduced { .. }
        ));
        assert_eq!(ledger.snapshot().unwrap().budget, requested);

        let clear_snapshot = ledger.snapshot().unwrap();
        let clear_slot = AllocationBundleSlot::new(1);
        let clear_claims = ledger.empty_retirement_claim_set();
        let clear_bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                clear_snapshot.policy_revision,
                vec![AllocationBundleDemand::new(
                    clear_slot,
                    AllocationClass::TransitionFrame,
                    1,
                )],
                clear_claims.clone(),
            ))
            .unwrap();
        let clear_bundle_id = ledger
            .snapshot()
            .unwrap()
            .active_replacement_bundle
            .unwrap();
        let clear_commit = clear_bundle
            .commit(vec![AllocationBundleActual::new(clear_slot, 1, 1)])
            .unwrap();
        let mut clear_retirement = Some(EmptyRetirementToken(clear_claims));
        let clear_prepared = clear_commit
            .prepare_retirement_settlement(&mut clear_retirement)
            .unwrap();

        engine.try_clear_cache_budget(requested).unwrap();
        let clear_pressure = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::BudgetPressure {
                    requested_budget,
                    clear_before_apply: true,
                    ..
                } if *requested_budget == requested
            )
        });
        assert!(matches!(
            clear_pressure,
            AlignmentSceneEngineEvent::BudgetPressure {
                requested_budget,
                clear_before_apply: true,
                pressure: LedgerError::ReplacementTransitionActive { bundle_id },
                ..
            } if requested_budget == requested && bundle_id == clear_bundle_id
        ));
        let clear_settled = clear_prepared.retire_and_settle();
        assert_eq!(clear_settled.len(), 1);
        engine.try_retry_budget_pressure(requested, true).unwrap();
        assert!(matches!(
            receive_matching(&engine, |event| matches!(
                event,
                AlignmentSceneEngineEvent::BudgetReduced { pressure }
                    if pressure.budget == requested
            )),
            AlignmentSceneEngineEvent::BudgetReduced { .. }
        ));
        assert_eq!(ledger.snapshot().unwrap().budget, requested);
    }

    #[test]
    fn active_gpu_settlement_defers_contract_replacement_then_publishes_once_on_exact_retry() {
        let (engine, mut contract, policy, camera, _) = test_engine();
        let owner = SourceOwnerId::new(44);
        let initial_request = AlignmentSceneRequestToken::new(80);
        let contract_request = AlignmentSceneRequestToken::new(81);
        engine
            .try_insert_lane_for_request(owner, initial_request, contract.clone(), policy)
            .unwrap();
        let _ = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });
        engine
            .try_update_camera_for_request(owner, initial_request, camera)
            .unwrap();
        let _ = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_token,
                    frame,
                    ..
                } if *seen == owner
                    && *seen_token == initial_request
                    && frame.visible_complete()
            )
        });

        let ledger = engine.ledger().clone();
        let snapshot = ledger.snapshot().unwrap();
        let slot = AllocationBundleSlot::new(0);
        let claims = ledger.empty_retirement_claim_set();
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                snapshot.policy_revision,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::TransitionFrame,
                    1,
                )],
                claims.clone(),
            ))
            .unwrap();
        let commit = bundle
            .commit(vec![AllocationBundleActual::new(slot, 1, 1)])
            .unwrap();
        let mut retirement = Some(EmptyRetirementToken(claims));
        let prepared = commit
            .prepare_retirement_settlement(&mut retirement)
            .unwrap();

        contract.presentation.style.show_direction_terminals = false;
        engine
            .try_replace_contract_for_request(owner, contract_request, contract)
            .unwrap();
        let pressure_event = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner: seen,
                    request_token: seen_token,
                    operation: AlignmentSceneEngineOperation::ReplaceContract,
                    pressure: LedgerError::ReplacementTransitionActive { .. },
                    ..
                } if *seen == owner && *seen_token == contract_request
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    request_token: Some(seen_token),
                    ..
                } if *seen == owner && *seen_token == contract_request
            )
        });
        let AlignmentSceneEngineEvent::ResourcePressure {
            target,
            intent,
            disposition,
            ..
        } = pressure_event
        else {
            panic!("ReplaceContract pressure became a terminal error: {pressure_event:?}")
        };
        assert_eq!(intent, SourceIntentKind::Foreground);
        assert_eq!(
            disposition,
            AlignmentSceneResourcePressureDisposition::Retryable
        );

        let settled = prepared.retire_and_settle();
        assert_eq!(settled.len(), 1);
        engine
            .try_retry_resource_pressure(owner, contract_request, target)
            .unwrap();
        let resumed = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_token,
                    cause: AlignmentSceneFrameCause::ResourcePressureRetry,
                    published_scene: Some(_),
                    frame,
                    ..
                } if *seen == owner
                    && *seen_token == contract_request
                    && frame.visible_complete()
                    && frame.snapshot.as_ref().is_some_and(|scene| {
                        !scene.reads().presentation().style.show_direction_terminals
                    })
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    request_token: Some(seen_token),
                    ..
                } if *seen == owner && *seen_token == contract_request
            )
        });
        assert!(
            matches!(resumed, AlignmentSceneEngineEvent::FrameUpdated { .. }),
            "the exact contract request must resume to one publication: {resumed:?}"
        );
        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "the resumed contract request published more than once"
        );
    }

    #[test]
    fn active_gpu_settlement_defers_optional_prewarm_without_error_or_busy_loop() {
        let (engine, contract, policy, camera, _) = test_engine();
        let owner = SourceOwnerId::new(45);
        let request_token = AlignmentSceneRequestToken::new(90);
        engine
            .try_insert_lane_for_request(owner, request_token, contract.clone(), policy)
            .unwrap();
        let _ = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });
        engine
            .try_update_camera_for_request(owner, request_token, camera)
            .unwrap();
        let _ = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_token,
                    frame,
                    ..
                } if *seen == owner
                    && *seen_token == request_token
                    && frame.visible_complete()
            )
        });

        let ledger = engine.ledger().clone();
        let snapshot = ledger.snapshot().unwrap();
        let slot = AllocationBundleSlot::new(0);
        let claims = ledger.empty_retirement_claim_set();
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                snapshot.policy_revision,
                vec![AllocationBundleDemand::new(
                    slot,
                    AllocationClass::TransitionFrame,
                    1,
                )],
                claims.clone(),
            ))
            .unwrap();
        let commit = bundle
            .commit(vec![AllocationBundleActual::new(slot, 1, 1)])
            .unwrap();
        let mut retirement = Some(EmptyRetirementToken(claims));
        let prepared = commit
            .prepare_retirement_settlement(&mut retirement)
            .unwrap();

        let mut prewarm = contract.presentation;
        prewarm.style.show_direction_terminals = false;
        engine.try_prewarm_presentation(owner, prewarm).unwrap();
        let deferred = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner: seen,
                    request_token: seen_token,
                    operation: AlignmentSceneEngineOperation::PrewarmPresentation,
                    pressure: LedgerError::ReplacementTransitionActive { .. },
                    ..
                } if *seen == owner && *seen_token == request_token
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    request_token: Some(seen_token),
                    ..
                } if *seen == owner && *seen_token == request_token
            )
        });
        let AlignmentSceneEngineEvent::ResourcePressure {
            intent,
            disposition,
            ..
        } = deferred
        else {
            panic!("PrewarmPresentation pressure became a terminal error: {deferred:?}")
        };
        assert_eq!(intent, SourceIntentKind::Speculative);
        assert_eq!(
            disposition,
            AlignmentSceneResourcePressureDisposition::Retryable
        );
        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "a deferred optional prewarm must not busy-poll"
        );

        let settled = prepared.retire_and_settle();
        assert_eq!(settled.len(), 1);
        engine.try_prewarm_presentation(owner, prewarm).unwrap();
        assert!(matches!(
            receive_matching(&engine, |event| matches!(
                event,
                AlignmentSceneEngineEvent::PresentationPrewarmed {
                    owner: seen,
                    prepared: true,
                } if *seen == owner
            )),
            AlignmentSceneEngineEvent::PresentationPrewarmed { prepared: true, .. }
        ));
    }

    #[test]
    fn worker_accepts_a_genomic_only_camera_without_synthetic_read_geometry() {
        let (engine, contract, policy, render_camera, _) = test_engine();
        let owner = SourceOwnerId::new(420);
        engine.try_insert_lane(owner, contract, policy).unwrap();
        receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });

        let camera = AlignmentSceneCamera::without_visible_rows(render_camera.viewport());
        engine.try_update_camera(owner, camera).unwrap();
        let complete = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated { owner: seen, frame, .. }
                    if *seen == owner && frame.visible_complete()
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated { frame, .. } = complete else {
            unreachable!("matching event has the asserted frame shape")
        };
        assert_eq!(frame.camera, camera);
        assert_eq!(frame.requested_rows, 0..0);
        assert_eq!(frame.covered_rows, 0..0);
        assert!(frame.snapshot.is_some());
    }

    #[test]
    fn owner_results_echo_the_exact_latest_request_token() {
        let (engine, contract, policy, camera, _) = test_engine();
        let owner = SourceOwnerId::new(421);
        let first = AlignmentSceneRequestToken::new(7);
        engine
            .try_insert_lane_for_request(owner, first, contract, policy)
            .unwrap();
        receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });
        engine
            .try_update_camera_for_request(owner, first, camera)
            .unwrap();
        let first_complete = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token,
                    frame,
                    ..
                } if *seen == owner && *request_token == first && frame.visible_complete()
            )
        });
        assert!(matches!(
            first_complete,
            AlignmentSceneEngineEvent::FrameUpdated {
                request_token,
                ..
            } if request_token == first
        ));

        // A same-camera refresh is a distinct desktop transaction even though the coordinator's
        // camera identity does not change. Its immediate frame must carry the new transaction.
        let second = AlignmentSceneRequestToken::new(8);
        engine
            .try_update_camera_for_request(owner, second, camera)
            .unwrap();
        let refreshed = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token,
                    cause: AlignmentSceneFrameCause::Camera,
                    frame,
                    ..
                } if *seen == owner && *request_token == second && frame.camera == camera
            )
        });
        assert!(matches!(
            refreshed,
            AlignmentSceneEngineEvent::FrameUpdated {
                request_token,
                ..
            } if request_token == second
        ));
    }

    #[test]
    fn worker_continues_incremental_vertical_residency_after_visible_publication() {
        let (engine, contract, _, camera, _) = test_engine();
        let owner = SourceOwnerId::new(43);
        let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
            .unwrap()
            .with_full_vertical_residency(true);
        engine.try_insert_lane(owner, contract, policy).unwrap();
        receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });
        engine.try_update_camera(owner, camera).unwrap();

        let complete = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    cause: AlignmentSceneFrameCause::VerticalResidency,
                    frame,
                    ..
                } if *seen == owner
                    && matches!(
                        frame.vertical_residency,
                        crate::AlignmentVerticalResidency::Complete { total_rows: 13 }
                    )
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated {
            frame, telemetry, ..
        } = complete
        else {
            unreachable!("matching event is a complete vertical frame")
        };
        assert!(frame.visible_complete());
        let scene = frame.snapshot.unwrap();
        assert_ne!(scene.reads().prepared_rows(), 0..13);
        assert!(telemetry.vertical_residency_checkpoints > 0);
        assert_eq!(
            telemetry.vertical_residency_blocks_warmed,
            telemetry.vertical_residency_checkpoints
        );
        assert_eq!(telemetry.vertical_residency_publications, 0);
        assert_eq!(telemetry.vertical_residency_completions, 1);
        let trace = telemetry
            .last_vertical_residency_pump
            .expect("the complete engine frame carries its semantic pump trace");
        assert_eq!(
            trace.outcome,
            AlignmentVerticalResidencyPumpOutcome::Completed
        );
        assert_eq!(trace.current_scene, scene.revision());
        assert_eq!(trace.published_scene, None);
        assert!(trace.target_prepared_rows.end <= 13);
        assert!(trace.target_read_pages.is_some_and(|pages| pages > 0));
        assert_eq!(trace.target_summary_pages, None);
        assert!(trace.pump_elapsed_nanoseconds >= trace.prepare_elapsed_nanoseconds);
    }

    #[test]
    fn oversized_optional_vertical_page_publishes_budget_limited_frame_not_pressure() {
        let (engine, contract, _, camera, _) = test_engine();
        let owner = SourceOwnerId::new(44);
        let request = AlignmentSceneRequestToken::new(91);
        let viewport_only = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
            .unwrap()
            .with_full_vertical_residency(false);
        engine
            .try_insert_lane(owner, contract, viewport_only)
            .unwrap();
        receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });
        engine
            .try_update_camera_for_request(owner, request, camera)
            .unwrap();
        let visible = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_request,
                    frame,
                    ..
                } if *seen == owner && *seen_request == request && frame.visible_complete()
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated { frame, .. } = visible else {
            unreachable!("the predicate accepts only a visible frame")
        };
        let visible_revision = frame.snapshot.unwrap().revision();

        engine.try_reduce_budget(1).unwrap();
        receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::BudgetReduced { pressure } if pressure.budget == 1
            )
        });
        let full_vertical = viewport_only.with_full_vertical_residency(true);
        engine
            .try_set_preparation_policy_for_request(owner, request, full_vertical)
            .unwrap();

        let outcome = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token: seen_request,
                    cause: AlignmentSceneFrameCause::VerticalResidency,
                    frame,
                    ..
                } if *seen == owner
                    && *seen_request == request
                    && matches!(
                        frame.vertical_residency,
                        crate::AlignmentVerticalResidency::BudgetLimited { .. }
                    )
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner: seen,
                    operation: AlignmentSceneEngineOperation::PumpPresentation,
                    ..
                } if *seen == owner
            ) || matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(seen),
                    operation: AlignmentSceneEngineOperation::SetPreparationPolicy,
                    ..
                } if *seen == owner
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated {
            frame,
            published_scene,
            ..
        } = outcome
        else {
            panic!(
                "optional vertical residency became pressure/error instead of a BudgetLimited frame: {outcome:?}"
            )
        };
        assert!(frame.visible_complete());
        assert_eq!(frame.snapshot.unwrap().revision(), visible_revision);
        assert!(published_scene.is_none());
    }

    #[test]
    fn unknown_owner_error_does_not_kill_the_source_worker() {
        let (engine, contract, policy, camera, _) = test_engine();
        let unknown = SourceOwnerId::new(99);
        engine.try_update_camera(unknown, camera).unwrap();
        assert!(matches!(
            receive_matching(&engine, |event| matches!(
                event,
                AlignmentSceneEngineEvent::Error {
                    owner: Some(owner),
                    operation: AlignmentSceneEngineOperation::UpdateCamera,
                    ..
                } if *owner == unknown
            )),
            AlignmentSceneEngineEvent::Error { .. }
        ));

        let live = SourceOwnerId::new(100);
        engine.try_insert_lane(live, contract, policy).unwrap();
        assert!(matches!(
            receive_matching(&engine, |event| matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner } if *owner == live
            )),
            AlignmentSceneEngineEvent::LaneInserted { .. }
        ));
    }

    #[test]
    fn exact_refusal_crosses_the_worker_boundary_and_limit_change_retries_once() {
        let (engine, contract, policy, camera, _) = test_engine();
        let owner = SourceOwnerId::new(111);
        engine.try_insert_lane(owner, contract, policy).unwrap();
        receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::LaneInserted { owner: seen } if *seen == owner
            )
        });

        engine
            .try_set_source_control(LocalBamSourceControl::SetCompleteReadLimit(
                NonZeroUsize::new(1).unwrap(),
            ))
            .unwrap();
        receive_matching(&engine, |event| {
            matches!(event, AlignmentSceneEngineEvent::SourceControlQueued)
        });
        engine.try_update_camera(owner, camera).unwrap();
        let refused = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::IndividualReadsRefused { owner: seen, .. }
                    if *seen == owner
            )
        });
        let AlignmentSceneEngineEvent::IndividualReadsRefused {
            target,
            complete_read_limit,
            eligible_records,
            frame,
            telemetry,
            ..
        } = refused
        else {
            unreachable!("the matching event has the asserted refusal shape")
        };
        assert_eq!(complete_read_limit, 1);
        assert_eq!(eligible_records, 13);
        assert_eq!(telemetry.source_refusals, 1);
        assert_eq!(
            frame.individual_reads,
            crate::AlignmentIndividualReadCapability::Refused {
                target,
                complete_read_limit,
                eligible_records,
            }
        );
        assert!(frame.individual_reads_refused_for_camera());

        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "a terminal refusal must leave the worker idle instead of producing a restart loop"
        );

        // Camera/clip frames published after the dedicated terminal edge must remain
        // self-describing. The desktop may legitimately consume only this later latest frame.
        let vertically_shifted = AlignmentCamera::new(
            camera.interval(),
            camera.row_scroll() + 1.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let later_request = AlignmentSceneRequestToken::new(7);
        engine
            .try_update_camera_for_request(owner, later_request, vertically_shifted)
            .unwrap();
        let later = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner: seen,
                    request_token,
                    frame,
                    ..
                } if *seen == owner
                    && *request_token == later_request
                    && frame.camera == vertically_shifted
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated { frame, .. } = later else {
            unreachable!("the matching event has the asserted frame shape")
        };
        assert!(frame.individual_reads_refused_for_camera());
        assert_eq!(
            frame.individual_reads,
            crate::AlignmentIndividualReadCapability::Refused {
                target,
                complete_read_limit,
                eligible_records,
            }
        );

        engine
            .try_set_source_control(LocalBamSourceControl::SetCompleteReadLimit(
                NonZeroUsize::new(1_000).unwrap(),
            ))
            .unwrap();
        receive_matching(&engine, |event| {
            matches!(event, AlignmentSceneEngineEvent::SourceControlQueued)
        });
        let completed = receive_matching(&engine, |event| {
            matches!(
                event,
                AlignmentSceneEngineEvent::FrameUpdated { owner: seen, frame, .. }
                    if *seen == owner && frame.visible_complete()
            )
        });
        let AlignmentSceneEngineEvent::FrameUpdated { frame, .. } = completed else {
            unreachable!("the matching event has the asserted completed-frame shape")
        };
        assert!(matches!(
            frame.individual_reads,
            crate::AlignmentIndividualReadCapability::Complete { .. }
        ));
    }

    #[test]
    fn local_bam_bootstrap_opens_on_the_worker_and_reports_scene_authority() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let engine = AlignmentSceneEngine::spawn_local_bam(LocalAlignmentSceneEngineConfig {
            path,
            open_options: AlignmentOpenOptions::default(),
            query_options: QueryOptions {
                max_records: NonZeroUsize::new(1_000).unwrap(),
                ..QueryOptions::default()
            },
            ledger: AllocationLedger::new_with_transient_budget(64 << 20, 32 << 20, 64 << 20),
            mailbox_control_capacity: NonZeroUsize::new(8).unwrap(),
            source_control_capacity: NonZeroUsize::new(4).unwrap(),
            maximum_source_control_burst: NonZeroUsize::new(2).unwrap(),
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for source open"
            );
            let Some(event) = engine.receive_timeout(Duration::from_millis(50)).unwrap() else {
                continue;
            };
            match event {
                AlignmentSceneEngineEvent::SourceOpened { metadata } => {
                    assert!(!metadata.dictionary.is_empty());
                    assert!(metadata.index_overview.is_some());
                    assert_ne!(metadata.source_revision, AlignmentSourceRevision::default());
                    assert_ne!(metadata.record_contract, ContractFingerprint::new(0));
                    break;
                }
                AlignmentSceneEngineEvent::Error { message, .. } => {
                    panic!("local BAM bootstrap failed: {message}")
                }
                _ => {}
            }
        }
    }

    fn receive_matching<B>(
        engine: &AlignmentSceneEngine<B>,
        predicate: impl Fn(&AlignmentSceneEngineEvent) -> bool,
    ) -> AlignmentSceneEngineEvent
    where
        B: CanonicalAlignmentSourceBackend + Send + 'static,
        B::Control: Send + 'static,
        B::Error: Error + Send + Sync + 'static,
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for scene event"
            );
            if let Some(event) = engine.receive_timeout(Duration::from_millis(50)).unwrap()
                && predicate(&event)
            {
                return event;
            }
        }
    }
}
