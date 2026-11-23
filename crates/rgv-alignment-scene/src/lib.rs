//! Immutable semantic authority for alignment evidence and presentation.
//!
//! This crate is deliberately headless. It owns source-stable handles, factorised contracts,
//! completeness certificates, canonical chunks, and immutable scene data. It does not perform
//! source I/O, create GPU resources, or know about desktop widgets.

mod acquisition;
mod allele_summary;
mod chunk;
mod contract;
mod coverage;
mod cpu_projector;
mod cpu_summary_projector;
mod depth_pyramid;
mod evidence;
mod filter_summary;
mod fingerprint;
mod identity;
mod interaction;
mod junction_annotation;
mod junction_summary;
mod modification_summary;
mod page_identity;
mod pair_summary;
mod panel_scene;
mod panel_summary;
mod payload;
mod presentation_scene;
mod presentation_store;
#[cfg(test)]
mod property_tests;
mod resource;
mod revision;
mod row_atlas;
mod row_oracle;
mod sampling;
mod scene_camera;
mod scene_snapshot;
mod semantic_page;
mod store;
mod summary;
mod summary_page;
mod summary_root;
mod tile;
mod view_filter;

pub use acquisition::{EvidenceAcquisition, EvidenceAcquisitionError};
pub use allele_summary::{ExactAlleleSummaryError, ExactAlleleSummaryKey, ExactAlleleSummaryTile};
pub use chunk::{CanonicalRecordChunk, ChunkError, SourceChunkId};
pub use contract::{
    AdmissionPolicy, AlignmentPageFamily, AlignmentReadLabelMode, AlignmentStyleContract,
    AlleleSummaryContract, AlleleSummaryDetail, CapabilityRequirement, DepthSummaryContract,
    FilterSummaryContract, GeometryLod, JunctionSummaryContract, LayoutContract,
    MAXIMUM_HIDDEN_MODIFICATION_CODES, ModificationSummaryContract, PairSummaryContract,
    PayloadContract, PresentationContract, RecordContract, SummaryContract,
};
pub use coverage::{
    CertifiedCoverageSet, CompleteCoverage, CoverageError, FilteredRecordProvenance,
};
pub use cpu_projector::{
    ARROWHEAD_LENGTH_FRACTION, AlignmentCamera, AlignmentHit, CpuAlignmentFrame,
    CpuAlignmentLabelFrame, CpuAlignmentProjector, CpuArrowhead, CpuClipRect, CpuFrameCoverage,
    CpuInteractionError, CpuPrimitiveShape, CpuProjectedPrimitive, CpuProjectionError,
    EVENT_LABEL_MINIMUM_POINTS_PER_BASE, arrowhead_covers, arrowhead_length, clip_edge_covers,
    clip_edge_extent, directional_extent, point_marker_extent,
};
pub use cpu_summary_projector::{
    CoverageAlleleInterpretation, CpuAlleleProjectionOptions, CpuAlleleToken,
    CpuBisulfiteStrandToken, CpuDepthProjectionOptions, CpuJunctionProjectionOptions,
    CpuJunctionScaleMeasurement, CpuJunctionStrandToken, CpuModificationProjectionOptions,
    CpuSummaryCamera, CpuSummaryFrame, CpuSummaryPrimitiveShape, CpuSummaryProjectedComponent,
    CpuSummaryProjectedPrimitive, CpuSummaryProjectionError, CpuSummaryProjectionOptions,
    CpuSummaryProjector,
};
pub use depth_pyramid::{
    DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH, DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES,
    DEPTH_PYRAMID_PRODUCTION_UNIT_BASES, DEPTH_PYRAMID_SUMMARY_SHARE_DIVISOR, DepthLaneAuthority,
    DepthPyramidColumn, DepthPyramidError, DepthPyramidLane, DepthPyramidPlan, DepthPyramidSegment,
    DepthPyramidStore, DepthPyramidTile, DepthPyramidView,
};
pub use evidence::{
    EvidenceBuildError, EvidenceChunk, EvidenceChunkError, EvidenceChunkFootprint, EvidencePins,
    EvidenceRecordRef, EvidenceRoot, EvidenceRootBuilder, EvidenceSelection,
    EvidenceSelectionError, EvidenceViewFilter,
};
pub use filter_summary::{ExactFilterSummaryError, ExactFilterSummaryKey, ExactFilterSummaryTile};
pub use fingerprint::ContractFingerprint;
pub use identity::AlignmentHandle;
pub use interaction::{AlignmentInteractionState, AlignmentInteractionToken};
pub use junction_annotation::{
    JunctionAnnotationEvent, JunctionAnnotationIndex, JunctionAnnotationIndexError,
    JunctionAnnotationInput, JunctionAnnotationPins, JunctionAnnotationStatus,
};
pub use junction_summary::{
    ExactJunctionSummaryError, ExactJunctionSummaryKey, ExactJunctionSummaryTile,
};
pub use modification_summary::{
    ExactModificationSummaryError, ExactModificationSummaryKey, ExactModificationSummaryTile,
};
pub use page_identity::{AlignmentPageContentToken, AlignmentPageIdentity};
pub use pair_summary::{
    ExactPairSummaryError, ExactPairSummaryKey, ExactPairSummaryTile, PairCandidateEvent,
    summarize_pair_candidates,
};
pub use panel_scene::{
    AlignmentCoherenceGroupFrame, AlignmentCoherenceGroupId, AlignmentCoherenceGroupKind,
    AlignmentCoherenceGroupSpec, AlignmentCoverageLaneOptions, AlignmentDerivedLaneCapture,
    AlignmentDerivedLaneKind, AlignmentDerivedLaneOptions, AlignmentModificationLaneOptions,
    AlignmentPanelLaneCapture, AlignmentPanelLaneFrame, AlignmentPanelLanePublication,
    AlignmentPanelSceneAdmission, AlignmentPanelSceneError, AlignmentPanelSceneFrame,
    AlignmentPanelSceneStore, AlignmentSashimiLaneOptions, AlignmentSashimiScalePolicy,
};
pub use panel_summary::{
    AlignmentPanelDerivedLaneFrame, AlignmentPanelDerivedLaneReadiness,
    AlignmentPanelDerivedLaneSummary, AlignmentPanelDerivedLaneUnavailable,
    AlignmentPanelSummaryFrame, AlignmentPanelSummaryProjector,
};
pub use payload::{PayloadSidecar, PayloadSidecarError};
pub use presentation_scene::{
    AlignmentExportError, AlignmentExportRecord, AlignmentPresentationScene,
    AlignmentPresentationSceneError, AlignmentTraversalDirection,
};
pub use presentation_store::{
    AlignmentPresentationOwner, AlignmentPresentationOwnerDemand, AlignmentPresentationStore,
    AlignmentPresentationStoreError, AlignmentReadPageReuseStats, AlignmentReadPageWarm,
    AlignmentSceneCandidate, PresentationCacheDemotion, PresentationCacheDiscard,
    PresentationStorePressure,
};
pub use resource::{
    AlignmentCacheResidencyClass, AlignmentCacheResidencyInventory, AlignmentMemoryPolicy,
    AllocationBundleAccountingOperation, AllocationBundleActual, AllocationBundleCommit,
    AllocationBundleDemand, AllocationBundleError, AllocationBundleLease,
    AllocationBundleReservation, AllocationBundleSlot, AllocationBundleSpec, AllocationClass,
    AllocationId, AllocationLease, AllocationLedger, AllocationOwner, AllocationOwnerShares,
    AllocationOwnerUsage, AllocationRecordSnapshot, AllocationReservation,
    AllocationRetirementClaim, AllocationRetirementClaimBuilder, AllocationRetirementClaimSet,
    AllocationRetirementToken, LedgerError, LedgerInventory, LedgerOperationCounters,
    LedgerProgress, LedgerProgressEpoch, LedgerProgressSubscription, LedgerRetainedBreakdown,
    LedgerSnapshot, PinnedAllocationLease, PreparedAllocationBundleSettlement, ReservationKind,
    TransientResourceClass, TransientResourceReservation, TransientResourceSnapshot,
};
pub use revision::{
    AlignmentSourceRevision, AnnotationRevision, EvidenceRevision, ReferenceRevision,
    RevisionFingerprint, RowAtlasRevision, RowCoordinateSpaceRevision, SceneRevision,
    SourceFingerprint, SummaryRevision,
};
pub use row_atlas::{
    AlignmentRowActivityBin, AlignmentRowActivityCursor, AlignmentRowActivityProfile,
    AlignmentRowActivityUpdateStats, AlignmentRowAtlas, AlignmentRowAtlasConstructionWork,
    AlignmentRowAtlasError, AlignmentRowAtlasPins, AlignmentRowConnector, RowAtlasRecord,
    RowAtlasRecordHit, RowExtent,
};
pub use row_oracle::{AlignmentRowGroup, AlignmentRowGroupKey, RowOracleError};
pub use sampling::ReadSampling;
pub use scene_camera::{AlignmentSceneCamera, AlignmentSceneCameraError, AlignmentSceneRowDemand};
pub use scene_snapshot::{
    AlignmentSceneAlleleSite, AlignmentScenePileupSite, AlignmentSceneSnapshot,
    AlignmentSceneSnapshotError, AlignmentSemanticRetirementStats,
    build_alignment_semantic_retirement_claim_set,
};
pub use semantic_page::{
    AlignmentBodyColorToken, AlignmentComponent, AlignmentGeometryToken, AlignmentLabelKind,
    AlignmentOperation, AlignmentPageKey, AlignmentSemanticLabel, AlignmentSemanticPage,
    AlignmentSemanticPrimitive, AlignmentStyleToken, BisulfiteObservationToken, CanonicalRowBlock,
    ClipEdgeToken, ModificationState, SbxInsertionToken, SemanticPageError, SemanticPageHit,
    SpecialistMode, SpecialistUnavailableReason, UltimaIndelToken, UltimaQualitySourceToken,
    UltimaUnavailableToken,
};
pub use store::{
    BoundedCacheTrim, EvidenceCandidate, EvidenceStore, EvidenceStoreError, ExactSummaryCandidate,
    RowAtlasCandidate, StorePressure,
};
pub use summary::{ExactDepthSummaryError, ExactDepthSummaryKey, ExactDepthSummaryTile};
pub use summary_page::{
    AnnotatedJunctionSemantic, SummaryAlleleReferenceContext, SummaryPageFamily, SummaryPageKey,
    SummarySemanticComponent, SummarySemanticPage, SummarySemanticPageError,
    SummarySemanticPrimitive,
};
pub use summary_root::{
    ExactAlleleSiteEvidence, ExactPileupSiteEvidence, ExactSummaryPins, ExactSummaryQueryError,
    ExactSummaryRoot, ExactSummaryRootError,
};
pub use tile::{CanonicalGenomicTile, CanonicalTileError};
pub use view_filter::{
    AlignmentFilterEvaluation, AlignmentFilterEvaluationError, AlignmentFilterPredicate,
    AlignmentFilterScope, AlignmentViewFilterContract, alignment_filter_reference_horizon,
    evaluate_alignment_filter,
};
