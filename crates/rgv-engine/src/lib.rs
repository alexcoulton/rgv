//! Bounded background scheduling and cache coordination for RGV.

mod alignment_engine;
mod cache;
mod mailbox;
mod reference_engine;
mod scheduler;
mod track_engine;

pub use alignment_engine::{
    AlignmentBrowsingPolicy, AlignmentBufferClampReason, AlignmentCacheOutcome, AlignmentEngine,
    AlignmentEngineEvent, AlignmentQueryPhase, AlignmentWorkingSetStats, EngineQueueError,
};
pub use cache::{
    AlignmentTileKey, CacheBudgets, CacheInsertError, CacheStats, CoverageTileKey, GpuTileKey,
    LayoutTileKey, SourceFingerprint, WeightedLruCache,
};
pub use reference_engine::{MAX_REFERENCE_QUERY_BASES, ReferenceEngine, ReferenceEngineEvent};
pub use scheduler::{CompletedWork, Scheduler, SchedulerError, WorkItem};
pub use track_engine::{
    TrackEngine, TrackEngineEvent, TrackSearchHit, TrackSearchResults, TrackSnapshot,
    TrackSnapshotData, TrackSpec, TrackSpecKind,
};
