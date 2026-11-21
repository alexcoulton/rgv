/// Physical alignment encoding whose coordinates are interpreted by a source adapter.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentSourceFormat {
    Bam,
    Cram,
}

/// Opaque identity of one validated alignment source generation.
///
/// `source` identifies the logical object. `revision` identifies the exact file/index snapshot
/// accepted when that object was opened. Callers may provide a previously persisted stamp when a
/// local copy and an HTTP object are known to be the same immutable resource; otherwise adapters
/// derive a conservative access-path-specific stamp automatically.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentSourceStamp {
    source: [u8; 16],
    revision: [u8; 16],
}

impl AlignmentSourceStamp {
    #[must_use]
    pub const fn new(source: [u8; 16], revision: [u8; 16]) -> Self {
        Self { source, revision }
    }

    #[must_use]
    pub const fn source(self) -> [u8; 16] {
        self.source
    }

    #[must_use]
    pub const fn revision(self) -> [u8; 16] {
        self.revision
    }
}
