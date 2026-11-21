//! Bounded background scheduling and cache coordination for RGV.

mod alignment_scene_coordinator;
mod alignment_scene_engine;
mod cache;
mod local_bam_source;
mod mailbox;
mod reference_engine;
mod source_actor;
mod track_engine;
mod track_source;

pub use alignment_scene_coordinator::{
    AlignmentCoordinatorEviction, AlignmentCoordinatorFrame, AlignmentCoordinatorPressure,
    AlignmentCoordinatorPump, AlignmentCoordinatorUpdate, AlignmentIndividualReadCapability,
    AlignmentPresentationPump, AlignmentReferenceAuthority, AlignmentReferenceInstallOutcome,
    AlignmentReferenceRequirement, AlignmentSceneBuildContract, AlignmentSceneCoordinator,
    AlignmentSceneCoordinatorConfigError, AlignmentSceneCoordinatorError,
    AlignmentSceneCoordinatorTelemetry, AlignmentSceneOwnerFailure, AlignmentSceneOwnerPressure,
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
pub use cache::SourceFingerprint;
pub use local_bam_source::{
    LocalBamCanonicalBackend, LocalBamCanonicalBackendError, LocalBamSourceControl,
};
pub use mailbox::{EngineQueueError, set_ui_waker};
pub use reference_engine::{
    MAX_REFERENCE_QUERY_BASES, MAX_REFERENCE_SLICE_BASES, ReferenceEngine, ReferenceEngineEvent,
    ReferenceQueryIdentity,
};
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
    TrackSnapshotData, TrackSourceDiagnostics, TrackSpec, TrackSpecKind,
};

#[cfg(test)]
mod retirement_tests;
