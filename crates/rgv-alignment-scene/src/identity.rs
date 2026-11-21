use rgv_hts::AlignmentSourcePosition;

use crate::AlignmentSourceRevision;

/// Stable identity of one physical alignment record within an immutable source revision.
///
/// # Size, and the planned compaction (SCENE-9)
///
/// A handle is 56 bytes: a 32-byte [`AlignmentSourceRevision`] and a 24-byte source position.
/// Every record of one source repeats the same revision, in each semantic primitive, row-atlas
/// record, evidence index entry and hit. The public shape is deliberately unchanged for now
/// because `rgv-engine` and the desktop construct and compare handles directly. The intended
/// change, to be made in one pass across those crates:
///
/// 1. Containers that hold records of exactly one source (`EvidenceRoot`, `AlignmentRowAtlas`,
///    `AlignmentSemanticPage`, `AlignmentPresentationScene`) own the `AlignmentSourceRevision`
///    once; each already knows it (`source_revision()` exists on the page key and identity).
/// 2. Inside those containers records keep a compact `AlignmentRecordRef` (the 24-byte source
///    position, or 16 bytes once the position enum is packed into a tagged `u64` pair).
///    `AlignmentSemanticPrimitive` then drops from 152 to about 120 bytes and `RowAtlasRecord`
///    by 32 bytes; `AlignmentComponent::PairConnector` already stores its mate this way.
/// 3. `AlignmentHandle` stays the public, self-describing identity at every boundary that can
///    mix sources or outlive a container (selection, interaction state, hit results, engine
///    messages). Containers hand handles out through accessors that re-attach their revision
///    (`container.handle(record_ref)`), and accept them by checking the revision once and then
///    comparing positions, so an old-revision handle still never matches a new source.
/// 4. Ordering and hashing are unaffected inside a container (the revision is constant there),
///    which keeps row-atlas and evidence sort orders, page content digests and the property
///    harness's reference model unchanged.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentHandle {
    source_revision: AlignmentSourceRevision,
    source_position: AlignmentSourcePosition,
}

impl AlignmentHandle {
    #[must_use]
    pub const fn new(
        source_revision: AlignmentSourceRevision,
        source_position: AlignmentSourcePosition,
    ) -> Self {
        Self {
            source_revision,
            source_position,
        }
    }

    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn source_position(self) -> AlignmentSourcePosition {
        self.source_position
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RevisionFingerprint, SourceFingerprint};

    fn source_revision(revision: u64) -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, revision),
        )
    }

    #[test]
    fn handle_distinguishes_physical_duplicates_and_source_revisions() {
        let first = AlignmentHandle::new(
            source_revision(4),
            AlignmentSourcePosition::bam_virtual_offset(100),
        );
        let duplicate = AlignmentHandle::new(
            source_revision(4),
            AlignmentSourcePosition::bam_virtual_offset(101),
        );
        let replaced_source = AlignmentHandle::new(
            source_revision(5),
            AlignmentSourcePosition::bam_virtual_offset(100),
        );

        assert_ne!(first, duplicate);
        assert_ne!(first, replaced_source);
    }
}
