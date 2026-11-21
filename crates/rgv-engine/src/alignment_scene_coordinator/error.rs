use std::sync::Arc;

use rgv_alignment_scene::{AlignmentPresentationStoreError, EvidenceStoreError};
use rgv_core::GenomicInterval;
use thiserror::Error;

use crate::{AlignmentSourceActorError, SourceOwnerId, SourceScheduleError};

/// Text of a lane-local scene-build failure. The backend error type is not required to be
/// displayable at this boundary, so a failure of the shared source itself yields `None` and
/// keeps its existing source-wide handling.
pub(crate) fn scene_build_failure_message<E>(
    error: &AlignmentSceneCoordinatorError<E>,
) -> Option<Arc<str>> {
    type Plain = AlignmentSceneCoordinatorError<std::convert::Infallible>;
    let message = match error {
        AlignmentSceneCoordinatorError::Source(_) => return None,
        AlignmentSceneCoordinatorError::Schedule(error) => error.to_string(),
        AlignmentSceneCoordinatorError::Store(error) => error.to_string(),
        AlignmentSceneCoordinatorError::Presentation(error) => error.to_string(),
        AlignmentSceneCoordinatorError::Config(error) => error.to_string(),
        AlignmentSceneCoordinatorError::CameraContigMismatch => {
            Plain::CameraContigMismatch.to_string()
        }
        AlignmentSceneCoordinatorError::UnknownOwner(owner) => {
            Plain::UnknownOwner(*owner).to_string()
        }
        AlignmentSceneCoordinatorError::CameraOutsideContig {
            interval,
            contig_length,
        } => Plain::CameraOutsideContig {
            interval: *interval,
            contig_length: *contig_length,
        }
        .to_string(),
        AlignmentSceneCoordinatorError::CameraSequenceOverflow => {
            Plain::CameraSequenceOverflow.to_string()
        }
        AlignmentSceneCoordinatorError::MissingCamera => Plain::MissingCamera.to_string(),
        AlignmentSceneCoordinatorError::IncompleteCachedHorizon(horizon) => {
            Plain::IncompleteCachedHorizon(*horizon).to_string()
        }
    };
    Some(Arc::from(message))
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum AlignmentSceneCoordinatorConfigError {
    #[error("alignment scene coordinator requires a non-empty contig")]
    EmptyContig,
    #[error("alignment scene source revision does not match its actor store")]
    SourceAuthorityMismatch,
    #[error("alignment scene record contract does not match its actor store")]
    RecordAuthorityMismatch,
    #[error("alignment scene source already contains owner {0:?}")]
    DuplicateOwner(SourceOwnerId),
    #[error("alignment scene source does not contain owner {0:?}")]
    UnknownOwner(SourceOwnerId),
    #[error("alignment presentation reference revision does not match its supplied bases")]
    ReferenceAuthorityMismatch,
    #[error("alignment presentation annotation revision does not match its supplied index")]
    AnnotationAuthorityMismatch,
    #[error("alignment reference bases do not cover the configured contig")]
    ReferenceCoverageMismatch,
    #[error("alignment preparation distances must be finite and non-negative")]
    InvalidPreparationDistance,
    #[error("alignment row coordinates exceed u32")]
    RowCoordinateOverflow,
    #[error(transparent)]
    Interval(#[from] rgv_core::IntervalError),
    #[error(transparent)]
    Tile(#[from] rgv_alignment_scene::CanonicalTileError),
}

#[derive(Debug, Error)]
pub enum AlignmentSceneCoordinatorError<E> {
    #[error(transparent)]
    Source(#[from] AlignmentSourceActorError<E>),
    #[error(transparent)]
    Schedule(#[from] SourceScheduleError<()>),
    #[error(transparent)]
    Store(#[from] EvidenceStoreError),
    #[error(transparent)]
    Presentation(#[from] AlignmentPresentationStoreError),
    #[error(transparent)]
    Config(#[from] AlignmentSceneCoordinatorConfigError),
    #[error("alignment camera belongs to a different contig")]
    CameraContigMismatch,
    #[error("alignment scene source does not contain owner {0:?}")]
    UnknownOwner(SourceOwnerId),
    #[error(
        "alignment camera interval {interval:?} exceeds configured contig length {contig_length}"
    )]
    CameraOutsideContig {
        interval: GenomicInterval,
        contig_length: u64,
    },
    #[error("alignment camera sequence space exhausted")]
    CameraSequenceOverflow,
    #[error("alignment scene coordinator has no live camera")]
    MissingCamera,
    #[error("cached evidence did not cover required semantic-page horizon {0:?}")]
    IncompleteCachedHorizon(GenomicInterval),
}
