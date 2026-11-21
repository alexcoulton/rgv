//! The RGV automation protocol and interaction stress tooling.
//!
//! The command, snapshot and telemetry wire types the desktop control server, the MCP server and
//! the control client exchange, plus the interaction stress scenario and recording documents.
//! They are built on the `rgv-core` domain types; `rgv-core` does not depend on this crate.

mod automation;
mod interaction_stress;

pub use automation::{
    AUTOMATION_SCHEMA_VERSION, AutomationAlignmentHit, AutomationAlignmentRelationship,
    AutomationAlignmentReveal, AutomationAlignmentSceneDiagnostics,
    AutomationAlignmentSearchResult, AutomationAlignmentSelector, AutomationAlignmentState,
    AutomationAlignmentTrackSnapshot, AutomationBatch, AutomationCommand, AutomationCommandResult,
    AutomationCommandStatus, AutomationContextLaneSnapshot, AutomationCytobandAvailability,
    AutomationEnvironmentTelemetry, AutomationError, AutomationErrorCode, AutomationEvidenceField,
    AutomationExportKind, AutomationFrameTelemetry, AutomationGenomeSnapshot,
    AutomationGpuTelemetry, AutomationHistoryEntrySnapshot, AutomationHistoryNavigation,
    AutomationInteractionTrace, AutomationInteractionTraceEvent, AutomationLandmarkIntegrityState,
    AutomationLandmarkSnapshot, AutomationLandmarkTrackState, AutomationNavigationDestination,
    AutomationNavigationHistorySnapshot, AutomationNavigationTelemetry, AutomationOptionScope,
    AutomationPaneItemLayoutSnapshot, AutomationPaneLayoutSnapshot, AutomationPanelSnapshot,
    AutomationPresentationCompositeSnapshot, AutomationPresentationGroupSnapshot,
    AutomationPresentationNodeSnapshot, AutomationReadPanelStateSnapshot,
    AutomationReadPresentationState, AutomationRectSnapshot, AutomationRenderLaneSnapshot,
    AutomationRenderOmissionReason, AutomationReviewItemSnapshot, AutomationReviewNavigation,
    AutomationReviewQueueSnapshot, AutomationRunReport, AutomationRunTelemetry,
    AutomationSelection, AutomationSelectionGranularity, AutomationSelectionKind,
    AutomationSelectionSnapshot, AutomationSetting, AutomationSnapshotFormat,
    AutomationSnapshotScope, AutomationTrackGestureKind, AutomationTrackGestureSnapshot,
    AutomationTrackLifecycle, AutomationTrackSnapshot, AutomationUnsavedChangesPolicy,
    AutomationViewportOperation, AutomationWorkspaceLayoutSnapshot,
    AutomationWorkspacePanelLayoutSnapshot, AutomationWorkspaceSnapshot,
    MAXIMUM_AUTOMATION_ALIGNMENT_RESULTS, MAXIMUM_AUTOMATION_COMMANDS,
    MAXIMUM_AUTOMATION_EVIDENCE_FIELDS, MAXIMUM_AUTOMATION_HISTORY_ENTRIES,
    MAXIMUM_AUTOMATION_LOCI, MAXIMUM_AUTOMATION_OPTION_BYTES,
    MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS, MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS,
    MAXIMUM_AUTOMATION_STRING_BYTES, MAXIMUM_AUTOMATION_TRACK_SELECTION,
    MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT, MINIMUM_AUTOMATION_SCHEMA_VERSION,
    MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS, check_automation_remote_url,
    check_session_sources_within,
};
pub use automation::{
    AutomationFeature, AutomationFeatureQueryResult, AutomationGeneHit,
    AutomationGeneNavigationResult, MAXIMUM_AUTOMATION_FEATURE_OFFSET,
    MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN, MAXIMUM_AUTOMATION_FEATURE_RESULTS,
    MAXIMUM_AUTOMATION_GENE_FLANK_BASES, MAXIMUM_AUTOMATION_GENE_QUERY_BYTES,
};
pub use automation::{
    MAXIMUM_AUTOMATION_SNAPSHOT_SCALE, MINIMUM_AUTOMATION_SNAPSHOT_SCALE, default_snapshot_scale,
};
pub use interaction_stress::{
    INTERACTION_RECORDING_SCHEMA_NAME, INTERACTION_RECORDING_SCHEMA_VERSION,
    INTERACTION_STRESS_SCHEMA_NAME, INTERACTION_STRESS_SCHEMA_VERSION, InteractionLifecycle,
    InteractionRecordingCheckpoint, InteractionRecordingEntry, InteractionRecordingFooter,
    InteractionRecordingHeader, InteractionRecordingState, InteractionRecordingStatus,
    InteractionStressError, InteractionStressRunManifest, InteractionStressScenario,
    InteractionStressStep, MAXIMUM_INTERACTION_STRESS_DOCUMENT_BYTES,
    MAXIMUM_STRESS_COORDINATE_FRACTION, MAXIMUM_STRESS_NATIVE_EVENTS, MAXIMUM_STRESS_NESTING,
    MAXIMUM_STRESS_REPEAT_COUNT, MAXIMUM_STRESS_RUN_MILLIS, MAXIMUM_STRESS_STEPS,
    MAXIMUM_STRESS_STRING_BYTES, MAXIMUM_STRESS_TIMING_SCALE, NativeEventShape,
    NativeInputObservation, NativeObservationSummary, RecordedReadScroll,
    STRESS_HEALTH_SCHEMA_VERSION, StressActionKind, StressArtifactReference, StressFailureClass,
    StressFailureSignature, StressGeneratorProfile, StressGeneratorSpecification,
    StressHealthSnapshot, StressInteractionState, StressModifiers, StressNativeEvent, StressPoint,
    StressPredicate, StressRequirements, StressRunEnvironment, StressRunLimits, StressRunState,
    StressScrollUnit, StressStepResult, StressTarget, StressTiming, StressTouchPhase,
};
