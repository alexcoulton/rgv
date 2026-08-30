use std::{
    collections::BTreeMap,
    error::Error,
    num::NonZeroUsize,
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use rgv_alignment_scene::{
    AlignmentCamera, AlignmentSourceRevision, AllocationLedger, ContractFingerprint, EvidenceStore,
    LedgerError, PresentationContract, SceneRevision,
};
use rgv_core::ReferenceDictionary;
use rgv_hts::{
    AlignmentIndexOverview, AlignmentOpenOptions, AlignmentSource, LocalAlignmentSource,
    QueryOptions,
};

use crate::{
    AlignmentCoordinatorFrame, AlignmentCoordinatorPressure, AlignmentCoordinatorUpdate,
    AlignmentReferenceAuthority, AlignmentSceneBuildContract, AlignmentSceneCoordinatorConfigError,
    AlignmentSceneCoordinatorTelemetry, AlignmentScenePreparationPolicy,
    AlignmentSceneSourceCoordinator, AlignmentSourceActor, CanonicalAlignmentSourceBackend,
    EngineQueueError, LocalBamCanonicalBackend, SourceIntentKind, SourceOwnerId,
    mailbox::{CoalescingReceiver, CoalescingSender, TryMailboxSendError, bounded_coalescing},
};

const WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
const MAXIMUM_INPUT_BURST: usize = 32;

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
    PumpSource,
    PumpPresentation,
}

#[derive(Clone, Debug)]
pub struct AlignmentSceneSourceMetadata {
    pub dictionary: ReferenceDictionary,
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
    SourcePublication,
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
        camera: AlignmentCamera,
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
        camera: AlignmentCamera,
    ) -> Result<(), EngineQueueError> {
        self.try_update_camera_for_request(owner, AlignmentSceneRequestToken::default(), camera)
    }

    /// Replaces any unread camera intent for this owner and associates every resulting event with
    /// the caller's exact request transaction.
    pub fn try_update_camera_for_request(
        &self,
        owner: SourceOwnerId,
        request_token: AlignmentSceneRequestToken,
        camera: AlignmentCamera,
    ) -> Result<(), EngineQueueError> {
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

fn scene_worker_loop<B>(
    mut coordinator: AlignmentSceneSourceCoordinator<B>,
    input: &CoalescingReceiver<AlignmentSceneEngineRequest<B::Control>>,
    output: &CoalescingSender<AlignmentSceneEngineEvent>,
) where
    B: CanonicalAlignmentSourceBackend,
    B::Error: Error + Send + Sync + 'static,
{
    let mut request_tokens = BTreeMap::new();
    loop {
        let first = if coordinator.has_pending_source_work()
            || coordinator.has_pending_presentation_work()
        {
            input.try_receive()
        } else {
            input.receive_timeout(WORKER_IDLE_WAIT)
        };
        match first {
            Ok(Some(request)) => {
                if !apply_request(&mut coordinator, &mut request_tokens, output, request) {
                    return;
                }
                for _ in 1..MAXIMUM_INPUT_BURST {
                    match input.try_receive() {
                        Ok(Some(request)) => {
                            if !apply_request(
                                &mut coordinator,
                                &mut request_tokens,
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

        if coordinator.has_pending_source_work() {
            match coordinator.pump_once() {
                Ok(pump) => {
                    for publication in pump.publications {
                        let Ok(Some(frame)) = coordinator.frame(publication.owner) else {
                            continue;
                        };
                        let Ok(telemetry) = coordinator.telemetry(publication.owner) else {
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
                        if output
                            .send_latest_for(
                                refusal.owner.get(),
                                AlignmentSceneEngineEvent::IndividualReadsRefused {
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
                                },
                            )
                            .is_err()
                        {
                            return;
                        }
                    }
                    for pressure in pump.pressures {
                        if pressure.intent == SourceIntentKind::Speculative {
                            // Optional scene-root read-ahead stops at resource pressure. The
                            // current immutable frame remains the owner's visible authority.
                            continue;
                        }
                        let message: Arc<str> = Arc::from(format!(
                            "alignment memory budget could not admit the requested scene roots: {}",
                            pressure.pressure
                        ));
                        if output
                            .send_control(AlignmentSceneEngineEvent::Error {
                                owner: Some(pressure.owner),
                                request_token: Some(current_request_token(
                                    &request_tokens,
                                    pressure.owner,
                                )),
                                operation: AlignmentSceneEngineOperation::PumpSource,
                                message,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(error) => {
                    if !handle_source_pump_error(&mut coordinator, &request_tokens, output, error) {
                        return;
                    }
                }
            }
        } else if coordinator.has_pending_presentation_work() {
            match coordinator.pump_presentation_once() {
                Ok(Some(pump)) => {
                    if !send_frame(
                        &coordinator,
                        output,
                        pump.owner,
                        current_request_token(&request_tokens, pump.owner),
                        AlignmentSceneFrameCause::VerticalResidency,
                        None,
                        pump.published_scene,
                    ) {
                        return;
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

/// Handles a failed bounded source checkpoint without widening an intent-local resource refusal
/// into a source-wide outage. The actor has already abandoned and cancelled the half-consumed
/// decoder transaction before this function runs.
fn handle_source_pump_error<B>(
    coordinator: &mut AlignmentSceneSourceCoordinator<B>,
    request_tokens: &BTreeMap<SourceOwnerId, AlignmentSceneRequestToken>,
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
        if work.kind() == SourceIntentKind::Speculative {
            // Optional read-ahead stops at resource pressure. The last immutable visible scene
            // remains authoritative, and a later camera intent may request the region normally.
            return true;
        }
        let message: Arc<str> = Arc::from(format!(
            "alignment memory budget could not admit the requested reads: {pressure}"
        ));
        return output
            .send_control(AlignmentSceneEngineEvent::Error {
                owner: Some(work.intent().owner()),
                request_token: Some(current_request_token(request_tokens, work.intent().owner())),
                operation: AlignmentSceneEngineOperation::PumpSource,
                message,
            })
            .is_ok();
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

fn allocation_pressure(error: &(dyn Error + 'static)) -> Option<LedgerError> {
    let mut current = Some(error);
    while let Some(candidate) = current {
        if let Some(pressure) = candidate.downcast_ref::<LedgerError>()
            && matches!(
                pressure,
                LedgerError::BudgetExceeded { .. } | LedgerError::TransientBudgetExceeded { .. }
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
            .or_else(|| allocation_pressure(error)),
        _ => allocation_pressure(error),
    }
}

fn apply_request<B>(
    coordinator: &mut AlignmentSceneSourceCoordinator<B>,
    request_tokens: &mut BTreeMap<SourceOwnerId, AlignmentSceneRequestToken>,
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
            request_tokens.insert(owner, request_token);
            match coordinator.update_camera(owner, camera) {
                Ok(update) => send_frame(
                    coordinator,
                    output,
                    owner,
                    request_token,
                    AlignmentSceneFrameCause::Camera,
                    Some(update),
                    update.published_scene,
                ),
                Err(error) => send_error(
                    output,
                    Some(owner),
                    Some(request_token),
                    AlignmentSceneEngineOperation::UpdateCamera,
                    error,
                ),
            }
        }
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
                Err(error) => send_error(
                    output,
                    Some(owner),
                    Some(request_token),
                    AlignmentSceneEngineOperation::ReplaceContract,
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
                Err(error) => send_error(
                    output,
                    Some(owner),
                    Some(request_token),
                    AlignmentSceneEngineOperation::InstallReferenceAuthority,
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
                Err(error) => send_error(
                    output,
                    Some(owner),
                    Some(request_token),
                    AlignmentSceneEngineOperation::SetPreparationPolicy,
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
            Err(error) => send_error(
                output,
                Some(owner),
                Some(current_request_token(request_tokens, owner)),
                AlignmentSceneEngineOperation::PrewarmPresentation,
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
            match coordinator.reduce_budget(budget) {
                Ok(pressure) => send_control(
                    output,
                    AlignmentSceneEngineEvent::BudgetReduced { pressure },
                ),
                Err(error) => send_error(
                    output,
                    None,
                    None,
                    AlignmentSceneEngineOperation::ReduceBudget,
                    error,
                ),
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
    output
        .send_latest_for(
            owner.get(),
            AlignmentSceneEngineEvent::FrameUpdated {
                owner,
                request_token,
                cause,
                update,
                published_scene,
                frame,
                telemetry,
            },
        )
        .is_ok()
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
        AlignmentStyleContract, AlleleSummaryDetail, AllocationLedger, CapabilityRequirement,
        EvidenceStore, GeometryLod, LayoutContract, PayloadContract, PresentationContract,
        RevisionFingerprint, SourceFingerprint, SummaryContract,
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
    use crate::{
        AlignmentSceneCoordinatorError, AlignmentSourceActor, AlignmentSourceActorError,
        AlignmentSourceIntentSpec, CanonicalDecodeCheckpoint, LocalBamCanonicalBackend,
        LocalBamCanonicalBackendError, LocalBamSourceControl, SourceWork,
    };

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
    fn foreground_allocation_pressure_is_owner_scoped_and_concrete() {
        let owner = SourceOwnerId::new(301);
        let (engine, checkpoints) = pressure_engine(owner, SourceIntentKind::Foreground);
        let event = receive_matching(&engine, |event| {
            matches!(event, AlignmentSceneEngineEvent::Error { .. })
        });
        let AlignmentSceneEngineEvent::Error {
            owner: seen,
            operation,
            message,
            ..
        } = event
        else {
            unreachable!("the matching event is an error")
        };
        assert_eq!(seen, Some(owner), "unexpected source error: {message}");
        assert_eq!(operation, AlignmentSceneEngineOperation::PumpSource);
        assert!(message.contains("alignment memory budget"));
        assert!(message.contains("requested 64"));
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
    fn speculative_allocation_pressure_pauses_without_an_unavailable_error() {
        let owner = SourceOwnerId::new(302);
        let (engine, checkpoints) = pressure_engine(owner, SourceIntentKind::Speculative);
        let deadline = Instant::now() + Duration::from_secs(5);
        while checkpoints.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "source pressure was never reached"
            );
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(50));
        assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
        assert!(engine.try_receive().unwrap().is_none());
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
            summaries: SummaryContract {
                overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
                minimum_base_quality: 0,
                minimum_junction_flank: 0,
                allele_detail: AlleleSummaryDetail::None,
                collect_modifications: false,
            },
            layout: LayoutContract {
                row_scale_class: 0,
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
        assert_eq!(frame.snapshot.unwrap().reads().prepared_rows(), 0..13);
        assert!(telemetry.vertical_residency_checkpoints > 0);
        assert_eq!(telemetry.vertical_residency_completions, 1);
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

        assert!(
            engine
                .receive_timeout(Duration::from_millis(50))
                .unwrap()
                .is_none(),
            "a terminal refusal must leave the worker idle instead of producing a restart loop"
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
