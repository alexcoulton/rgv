//! Bounded background scheduling and cache coordination for RGV.

mod alignment_coverage_engine;
mod alignment_scene_coordinator;
mod alignment_scene_engine;
mod cache;
mod decode_allowance;
mod local_bam_source;
mod mailbox;
mod reference_engine;
mod source_actor;
mod track_engine;
mod track_source;
mod track_tile_engine;
mod track_tile_source;

pub use alignment_coverage_engine::{
    ALIGNMENT_COVERAGE_DEFAULT_RESIDENT_BYTES, ALIGNMENT_COVERAGE_REMOTE_UNIT_TRANSFER_BUDGET,
    AlignmentCoverageEngine, AlignmentCoverageEngineConfig, AlignmentCoveragePublication,
    AlignmentCoverageRequest, AlignmentCoverageState, AlignmentCoverageTelemetry,
};
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
pub use decode_allowance::{
    AlignmentDecodeAllowanceSnapshot, alignment_decode_allowance, set_alignment_decode_concurrency,
};
#[allow(deprecated)]
pub use local_bam_source::LocalBamCanonicalBackend;
pub use local_bam_source::{
    AlignmentDecodeTelemetry, LocalBamCanonicalBackendError, LocalBamSourceControl,
    PositionedAlignmentBackend,
};
pub use mailbox::{EngineQueueError, set_ui_waker};
pub use reference_engine::{
    MAX_REFERENCE_QUERY_BASES, MAX_REFERENCE_SLICE_BASES, MAX_SPLICE_MOTIF_QUERY_INTRONS,
    ReferenceEngine, ReferenceEngineEvent, ReferenceQueryIdentity, ReferenceSliceDelivery,
    ReferenceSliceSink, SpliceMotifQuery,
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
pub use track_tile_engine::{
    DEFAULT_TRACK_TILE_CACHE_BYTES, LaidOutTrackTile, MAXIMUM_TRACK_TILE_HELPERS,
    TrackTileCacheStats, TrackTileEngine, TrackTileEngineConfig, TrackTileEngineEvent,
    TrackTileView, TrackTileViewSnapshot,
};
pub use track_tile_source::{
    MAXIMUM_CLUSTER_CLOSURE_ROUNDS, MAXIMUM_CLUSTER_CLOSURE_TILES, TrackTileLayoutFn,
    TrackTileLayoutRegistry, TrackTileSearch, TrackTileSearchHits, TrackTileSource,
};

#[cfg(test)]
mod retirement_tests;
