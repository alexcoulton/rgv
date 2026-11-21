use std::{ops::Range, sync::Arc};

use rgv_alignment_scene::{
    AlignmentRowActivityProfile, AlignmentSceneCamera, AlignmentSceneSnapshot, SceneRevision,
};
use rgv_core::GenomicInterval;

use super::AlignmentReferenceRequirement;

/// Observable state of the optional complete vertical residency pass. `Complete` means that a
/// camera-only vertical move can select any row page from the published immutable scene without
/// source, layout, or semantic-page work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentVerticalResidency {
    #[default]
    ViewportBounded,
    Preparing {
        prepared_rows: u32,
        total_rows: u32,
    },
    Complete {
        total_rows: u32,
    },
    BudgetLimited {
        prepared_rows: u32,
        total_rows: u32,
    },
    /// The visible scene remains complete, but an unexpected optional page-warm failure stopped
    /// the background pass for this scene authority. A later source horizon or contract may retry.
    Unavailable {
        prepared_rows: u32,
        total_rows: u32,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CameraDirection {
    #[default]
    Stationary,
    Left,
    Right,
}

#[derive(Clone, Debug)]
pub(crate) struct DesiredScene {
    pub(crate) camera: AlignmentSceneCamera,
    pub(crate) camera_sequence: u64,
    pub(crate) prepared_interval: GenomicInterval,
    pub(crate) source_horizon: GenomicInterval,
    pub(crate) prepared_rows: Option<Range<u32>>,
    pub(crate) direction: CameraDirection,
}

/// Current visible publication. A missing `covered_interval` or a strict subset means only that
/// edge is loading; the retained snapshot remains available and is never replaced by a blank
/// whole-lane sentinel.
#[derive(Clone, Debug)]
pub struct AlignmentCoordinatorFrame {
    pub camera: AlignmentSceneCamera,
    pub camera_sequence: u64,
    pub snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    pub covered_interval: Option<GenomicInterval>,
    pub requested_rows: Range<u32>,
    pub covered_rows: Range<u32>,
    /// A camera-exact, bounded vertical activity summary derived from the immutable row atlas in
    /// `snapshot`. This is deliberately independent of atlas-wide vertical residency totals: the
    /// scrollbar describes rows active at the live locus while the cache may retain many more
    /// rows for immediate horizontal navigation.
    pub row_activity: Option<Arc<AlignmentRowActivityProfile>>,
    pub reference_requirement: AlignmentReferenceRequirement,
    /// Whether the retained snapshot was built from the lane's currently desired
    /// reference-independent presentation, summary, and layout contracts. Reference-backed
    /// interpretation has its own readiness bit below: a pending FASTA slice must not make
    /// truthful read geometry or exact depth stale.
    pub scene_contracts_current: bool,
    /// Whether every reference-dependent presentation product in the desired contract is also
    /// current. `false` is an optional-product state, not a reason to withhold the core scene.
    pub reference_products_current: bool,
    pub individual_reads: AlignmentIndividualReadCapability,
    pub vertical_residency: AlignmentVerticalResidency,
}

/// Constant-size proof of which worker queue can still advance one owner. A frame which is not
/// terminal and reports both fields false is ownerless rather than merely "still updating".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSceneOwnerWorkState {
    pub source_pending: bool,
    pub presentation_pending: bool,
}

impl AlignmentCoordinatorFrame {
    #[must_use]
    pub fn visible_complete(&self) -> bool {
        self.scene_contracts_current
            && self
                .covered_interval
                .is_some_and(|covered| covered.contains_interval(self.camera.interval()))
            && self.covered_rows.start <= self.requested_rows.start
            && self.covered_rows.end >= self.requested_rows.end
    }

    /// Whether the source has made a terminal individual-read decision for this frame's camera.
    /// A retained refusal from another request is not authority merely because it is attached to
    /// a newer frame: its counted source target must contain the exact camera interval.
    #[must_use]
    pub fn individual_reads_refused_for_camera(&self) -> bool {
        matches!(
            self.individual_reads,
            AlignmentIndividualReadCapability::Refused { target, .. }
                if target.contains_interval(self.camera.interval())
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentIndividualReadCapability {
    Pending,
    Complete {
        scene_revision: SceneRevision,
    },
    Refused {
        target: GenomicInterval,
        complete_read_limit: usize,
        eligible_records: u64,
    },
}
