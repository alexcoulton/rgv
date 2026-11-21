//! The one place which decides whether a failure is recoverable allocation pressure.
//!
//! Every alignment error enum wraps its inner errors with `#[error(transparent)]`, which makes
//! `Error::source` skip the wrapped value, so a `source()` walk cannot find a nested
//! [`LedgerError`]. Classification is therefore typed: [`budget_pressure`] is the single rule and
//! each wrapper only names the variants which can carry it. These impls belong on the error types
//! in `rgv-alignment-scene`; they live here as an extension trait until they move upstream.

use rgv_alignment_scene::{
    AlignmentFilterEvaluationError, AlignmentPresentationSceneError,
    AlignmentPresentationStoreError, AlignmentRowAtlasError, AlignmentSceneSnapshotError,
    EvidenceBuildError, EvidenceSelectionError, EvidenceStoreError, ExactAlleleSummaryError,
    ExactDepthSummaryError, ExactFilterSummaryError, ExactJunctionSummaryError,
    ExactModificationSummaryError, ExactPairSummaryError, ExactSummaryRootError, LedgerError,
    RowOracleError, SemanticPageError, SummarySemanticPageError,
};

use super::AlignmentSceneCoordinatorError;
use crate::CanonicalAlignmentSourceBackend;

/// The single rule: a refusal which shared-ledger progress (eviction, a larger budget, or a
/// settled GPU replacement) can clear. Bookkeeping corruption is never pressure.
pub(crate) const fn budget_pressure(error: &LedgerError) -> Option<LedgerError> {
    match error {
        LedgerError::BudgetExceeded { .. }
        | LedgerError::TransientBudgetExceeded { .. }
        | LedgerError::ReplacementTransitionActive { .. } => Some(*error),
        LedgerError::ReservationExceeded { .. }
        | LedgerError::UnknownAllocation(_)
        | LedgerError::IdOverflow
        | LedgerError::Poisoned => None,
    }
}

/// Recoverable allocation pressure carried by an error, however deeply it is wrapped.
pub(crate) trait AllocationPressure {
    fn allocation_pressure(&self) -> Option<LedgerError>;
}

impl AllocationPressure for LedgerError {
    fn allocation_pressure(&self) -> Option<LedgerError> {
        budget_pressure(self)
    }
}

/// Implements [`AllocationPressure`] for a wrapper by naming the variants whose payload can carry
/// pressure. Every other variant is a genuine failure.
macro_rules! pressure_through {
    ($($wrapper:ty => [$($variant:ident),+ $(,)?];)+) => {$(
        impl AllocationPressure for $wrapper {
            fn allocation_pressure(&self) -> Option<LedgerError> {
                #[allow(unreachable_patterns, clippy::match_wildcard_for_single_variants)]
                match self {
                    $(Self::$variant(inner) => inner.allocation_pressure(),)+
                    _ => None,
                }
            }
        }
    )+};
}

pressure_through! {
    EvidenceSelectionError => [Ledger];
    EvidenceBuildError => [Ledger, Selection];
    ExactDepthSummaryError => [Ledger, Evidence];
    ExactAlleleSummaryError => [Ledger, Evidence];
    ExactJunctionSummaryError => [Ledger, Evidence];
    ExactPairSummaryError => [Ledger, Evidence];
    ExactModificationSummaryError => [Ledger, Evidence];
    ExactFilterSummaryError => [Ledger];
    ExactSummaryRootError => [Ledger];
    RowOracleError => [Evidence];
    AlignmentRowAtlasError => [Ledger, EvidenceSelection, RowOracle];
    AlignmentFilterEvaluationError => [Evidence];
    EvidenceStoreError => [
        Build,
        Ledger,
        ExactDepthSummary,
        ExactAlleleSummary,
        ExactJunctionSummary,
        ExactPairSummary,
        ExactModificationSummary,
        ExactFilterSummary,
        ExactSummaryRoot,
        RowAtlas,
        ViewFilter,
    ];
    SemanticPageError => [Ledger, EvidenceSelection];
    SummarySemanticPageError => [Ledger];
    AlignmentPresentationSceneError => [Ledger];
    AlignmentSceneSnapshotError => [Ledger];
    AlignmentPresentationStoreError => [Ledger, SemanticPage, SummaryPage, ReadScene, Snapshot];
}

/// Shared-ledger admission pressure from a lane's own scene work (store roots and semantic
/// pages). A failure of the shared source is deliberately not classified here: only the backend
/// knows which of its errors are pressure, see [`source_allocation_pressure`].
pub(crate) fn coordinator_budget_pressure<E>(
    error: &AlignmentSceneCoordinatorError<E>,
) -> Option<LedgerError> {
    match error {
        AlignmentSceneCoordinatorError::Store(error) => error.allocation_pressure(),
        AlignmentSceneCoordinatorError::Presentation(error) => error.allocation_pressure(),
        AlignmentSceneCoordinatorError::Source(_)
        | AlignmentSceneCoordinatorError::Schedule(_)
        | AlignmentSceneCoordinatorError::Config(_)
        | AlignmentSceneCoordinatorError::CameraContigMismatch
        | AlignmentSceneCoordinatorError::UnknownOwner(_)
        | AlignmentSceneCoordinatorError::CameraOutsideContig { .. }
        | AlignmentSceneCoordinatorError::CameraSequenceOverflow
        | AlignmentSceneCoordinatorError::MissingCamera
        | AlignmentSceneCoordinatorError::IncompleteCachedHorizon(_) => None,
    }
}

/// [`coordinator_budget_pressure`] extended with the backend's own classification of a source
/// failure. This is the classification the threaded engine uses for every worker error.
pub(crate) fn source_allocation_pressure<B>(
    error: &AlignmentSceneCoordinatorError<B::Error>,
) -> Option<LedgerError>
where
    B: CanonicalAlignmentSourceBackend,
{
    match error {
        AlignmentSceneCoordinatorError::Source(source) => {
            source.backend_error().and_then(B::allocation_pressure)
        }
        _ => coordinator_budget_pressure(error),
    }
}
