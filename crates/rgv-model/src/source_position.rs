/// Stable physical position of one alignment record in an immutable source revision.
///
/// This is deliberately separate from biological record content. Two byte-identical records at
/// different physical positions are different evidence and therefore receive different scene
/// handles. The source revision is carried by the higher-level scene handle.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentSourcePosition {
    /// The BGZF virtual offset immediately after a BAM record.
    ///
    /// The ending boundary is exposed consistently by both HTSlib and the positioned Noodles
    /// decoder, including when an indexed iterator skips non-intersecting records internally.
    BamVirtualOffset(u64),
    /// A CRAM record addressed by its container, slice landmark, and ordinal within that slice.
    CramRecord {
        container_offset: u64,
        slice_landmark: u64,
        record_index: u32,
    },
}

impl AlignmentSourcePosition {
    #[must_use]
    pub const fn bam_virtual_offset(offset: u64) -> Self {
        Self::BamVirtualOffset(offset)
    }

    #[must_use]
    pub const fn cram_record(
        container_offset: u64,
        slice_landmark: u64,
        record_index: u32,
    ) -> Self {
        Self::CramRecord {
            container_offset,
            slice_landmark,
            record_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_duplicates_remain_distinct() {
        assert_ne!(
            AlignmentSourcePosition::bam_virtual_offset(100),
            AlignmentSourcePosition::bam_virtual_offset(101)
        );
        assert_ne!(
            AlignmentSourcePosition::cram_record(10, 20, 0),
            AlignmentSourcePosition::cram_record(10, 20, 1)
        );
    }
}
