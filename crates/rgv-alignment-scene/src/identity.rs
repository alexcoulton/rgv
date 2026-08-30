use rgv_hts::AlignmentSourcePosition;

use crate::AlignmentSourceRevision;

/// Stable identity of one physical alignment record within an immutable source revision.
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
