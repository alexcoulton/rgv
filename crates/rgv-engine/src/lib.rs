//! Bounded background scheduling and cache coordination for RGV.

mod alignment_scene_coordinator;
mod alignment_scene_engine;
mod cache;
mod local_bam_source;
mod mailbox;
mod reference_engine;
mod scheduler;
mod source_actor;
mod track_engine;

pub use alignment_scene_coordinator::{
    AlignmentCoordinatorEviction, AlignmentCoordinatorFrame, AlignmentCoordinatorPressure,
    AlignmentCoordinatorPump, AlignmentCoordinatorUpdate, AlignmentIndividualReadCapability,
    AlignmentPresentationPump, AlignmentReferenceAuthority, AlignmentReferenceInstallOutcome,
    AlignmentReferenceRequirement, AlignmentSceneBuildContract, AlignmentSceneCoordinator,
    AlignmentSceneCoordinatorConfigError, AlignmentSceneCoordinatorError,
    AlignmentSceneCoordinatorTelemetry, AlignmentSceneOwnerPressure,
    AlignmentSceneOwnerPublication, AlignmentSceneOwnerRefusal, AlignmentSceneOwnerRetry,
    AlignmentSceneOwnerWorkState, AlignmentScenePreparationPolicy, AlignmentSceneSourceCoordinator,
    AlignmentSceneSourcePump, AlignmentVerticalResidency,
    alignment_row_activity_satisfies_contract,
};
pub use alignment_scene_engine::{
    AlignmentSceneEngine, AlignmentSceneEngineEvent, AlignmentSceneEngineOperation,
    AlignmentSceneFrameCause, AlignmentSceneRequestToken,
    AlignmentSceneResourcePressureDisposition, AlignmentSceneSourceMetadata,
    LocalAlignmentSceneEngineConfig,
};
pub use cache::{
    AlignmentTileKey, CacheBudgets, CacheInsertError, CacheStats, CoverageTileKey, GpuTileKey,
    LayoutTileKey, SourceFingerprint, WeightedLruCache,
};
pub use local_bam_source::{
    LocalBamCanonicalBackend, LocalBamCanonicalBackendError, LocalBamSourceControl,
};
pub use mailbox::EngineQueueError;
pub use reference_engine::{
    MAX_REFERENCE_QUERY_BASES, ReferenceEngine, ReferenceEngineEvent, ReferenceQueryIdentity,
};
pub use scheduler::{CompletedWork, Scheduler, SchedulerError, WorkItem};
pub use source_actor::{
    AlignmentSourceActor, AlignmentSourceActorAction, AlignmentSourceActorError,
    AlignmentSourceActorEvent, AlignmentSourceActorSchedule, AlignmentSourceIntent,
    AlignmentSourceIntentSpec, AlignmentSourceWorkError, CanonicalAlignmentSourceBackend,
    CanonicalDecodeCheckpoint, CompletedCanonicalChunk, CompletedChunkDisposition,
    CompletedSourceWork, SourceCheckpointDecision, SourceIntentKind, SourceIntentSequence,
    SourceOwnerId, SourceScheduleError, SourceWork, SourceWorkId, SourceYieldReason,
};
pub use track_engine::{
    TrackEngine, TrackEngineEvent, TrackSearchHit, TrackSearchResults, TrackSnapshot,
    TrackSnapshotData, TrackSpec, TrackSpecKind,
};
