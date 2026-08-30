/// Stable 128-bit revision fingerprint.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RevisionFingerprint {
    high: u64,
    low: u64,
}

impl RevisionFingerprint {
    #[must_use]
    pub const fn new(high: u64, low: u64) -> Self {
        Self { high, low }
    }

    #[must_use]
    pub const fn high(self) -> u64 {
        self.high
    }

    #[must_use]
    pub const fn low(self) -> u64 {
        self.low
    }
}

macro_rules! revision_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(RevisionFingerprint);

        impl $name {
            #[must_use]
            pub const fn new(value: RevisionFingerprint) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn fingerprint(self) -> RevisionFingerprint {
                self.0
            }
        }
    };
}

revision_type!(SourceFingerprint);
revision_type!(ReferenceRevision);
revision_type!(AnnotationRevision);
revision_type!(EvidenceRevision);
revision_type!(SummaryRevision);
revision_type!(RowAtlasRevision);
revision_type!(SceneRevision);

/// Identity of one immutable version of an alignment source.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentSourceRevision {
    source: SourceFingerprint,
    revision: RevisionFingerprint,
}

impl AlignmentSourceRevision {
    #[must_use]
    pub const fn new(source: SourceFingerprint, revision: RevisionFingerprint) -> Self {
        Self { source, revision }
    }

    #[must_use]
    pub const fn source(self) -> SourceFingerprint {
        self.source
    }

    #[must_use]
    pub const fn revision(self) -> RevisionFingerprint {
        self.revision
    }
}

impl From<rgv_hts::AlignmentSourceStamp> for AlignmentSourceRevision {
    fn from(value: rgv_hts::AlignmentSourceStamp) -> Self {
        Self::new(
            SourceFingerprint::new(fingerprint_from_bytes(value.source())),
            fingerprint_from_bytes(value.revision()),
        )
    }
}

fn fingerprint_from_bytes(bytes: [u8; 16]) -> RevisionFingerprint {
    let mut high = [0; 8];
    high.copy_from_slice(&bytes[..8]);
    let mut low = [0; 8];
    low.copy_from_slice(&bytes[8..]);
    RevisionFingerprint::new(u64::from_le_bytes(high), u64::from_le_bytes(low))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hts_stamp_conversion_preserves_all_bits() {
        let stamp = rgv_hts::AlignmentSourceStamp::new(
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            [
                16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
            ],
        );
        let revision = AlignmentSourceRevision::from(stamp);
        assert_eq!(
            revision.source().fingerprint(),
            RevisionFingerprint::new(
                u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]),
                u64::from_le_bytes([8, 9, 10, 11, 12, 13, 14, 15]),
            )
        );
        assert_eq!(
            revision.revision(),
            RevisionFingerprint::new(
                u64::from_le_bytes([16, 17, 18, 19, 20, 21, 22, 23]),
                u64::from_le_bytes([24, 25, 26, 27, 28, 29, 30, 31]),
            )
        );
    }
}
