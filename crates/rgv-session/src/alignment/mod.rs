//! Alignment sources as the session holds them: one controller per alignment track with its
//! scene and coverage engines, per-panel projections, filters and navigation transactions.
//! Nothing here names a window or a GPU device; GPU publication of the scenes stays with the
//! host.

pub mod admission;
pub mod coverage;
pub mod dispatch;
pub mod events;
pub mod lanes;
pub mod navigation;
pub mod settlement;
pub mod track;
