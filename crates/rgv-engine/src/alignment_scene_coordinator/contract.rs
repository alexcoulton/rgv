use std::{
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentRowActivityProfile, AlignmentSceneSnapshot, AlignmentSourceRevision,
    AlignmentViewFilterContract, ContractFingerprint, JunctionAnnotationIndex, LayoutContract,
    PayloadContract, PresentationContract, ReferenceRevision, SceneRevision, SummaryContract,
};
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::ReferenceSlice;

use super::AlignmentSceneCoordinatorConfigError;

pub(crate) const ALLELE_REFERENCE_CONTEXT_RADIUS: u64 = 2;

/// A presentation reference carries its immutable identity beside the actual bases. Keeping the
/// revision separate from `ReferenceSlice` prevents a caller from pairing current bases with a
/// stale presentation contract during scene publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentReferenceAuthority {
    pub(crate) revision: ReferenceRevision,
    pub(crate) slice: Arc<ReferenceSlice>,
}

impl AlignmentReferenceAuthority {
    #[must_use]
    pub const fn new(revision: ReferenceRevision, slice: Arc<ReferenceSlice>) -> Self {
        Self { revision, slice }
    }

    #[must_use]
    pub const fn revision(&self) -> ReferenceRevision {
        self.revision
    }

    #[must_use]
    pub const fn slice(&self) -> &Arc<ReferenceSlice> {
        &self.slice
    }
}

/// The exact reference dependency for the lane's currently desired source horizon. A pending
/// requirement is not a source failure: evidence, summaries, and layout may still be acquired and
/// retained while the reference engine catches up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentReferenceRequirement {
    #[default]
    NotRequired,
    Pending {
        revision: ReferenceRevision,
        horizon: GenomicInterval,
    },
    Ready {
        revision: ReferenceRevision,
        horizon: GenomicInterval,
    },
}

impl AlignmentReferenceRequirement {
    #[must_use]
    pub const fn is_ready(self) -> bool {
        !matches!(self, Self::Pending { .. })
    }
}

/// Result of offering immutable reference authority to one owner lane. Stale revisions and slices
/// which do not cover the lane's exact live horizon are ordinary ignored offers, not errors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentReferenceInstallOutcome {
    pub installed: bool,
    pub published_scene: Option<SceneRevision>,
}

/// Factorised authority and semantic contracts for one alignment lane. Camera state is
/// deliberately absent: pan and vertical scroll do not change this value or any page key.
#[derive(Clone, Debug)]
pub struct AlignmentSceneBuildContract {
    pub source_revision: AlignmentSourceRevision,
    pub record_contract: ContractFingerprint,
    pub contig: ContigId,
    pub contig_length: u64,
    /// Maximum number of eligible records which may be exposed in the exact live viewport.
    /// A wider immutable browsing horizon may retain more records under the separate byte budget.
    pub complete_read_limit: NonZeroUsize,
    pub payload: PayloadContract,
    pub view_filter: AlignmentViewFilterContract,
    /// Non-destructive overlay predicates do not change rows or summaries, but reference-backed
    /// highlights still need the same certified reference horizon as a true view filter.
    pub highlight_filter: AlignmentViewFilterContract,
    pub summaries: SummaryContract,
    pub layout: LayoutContract,
    pub presentation: PresentationContract,
    pub geometry_tile_bases: NonZeroU64,
    pub summary_tile_bases: NonZeroU64,
    pub row_block_rows: NonZeroU32,
    pub normalization_mapped_records: Option<u64>,
    pub reference: Option<AlignmentReferenceAuthority>,
    pub annotation: Option<Arc<JunctionAnnotationIndex>>,
}

impl AlignmentSceneBuildContract {
    pub(crate) fn view_filter_evaluation_fingerprint(&self) -> ContractFingerprint {
        self.view_filter
            .evaluation_fingerprint(self.presentation.reference)
    }

    pub(crate) fn validate(&self) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        if self.contig_length == 0 {
            return Err(AlignmentSceneCoordinatorConfigError::EmptyContig);
        }
        if self
            .reference
            .as_ref()
            .is_some_and(|reference| self.presentation.reference != Some(reference.revision))
        {
            return Err(AlignmentSceneCoordinatorConfigError::ReferenceAuthorityMismatch);
        }
        if self.presentation.annotation != self.annotation.as_ref().map(|value| value.revision()) {
            return Err(AlignmentSceneCoordinatorConfigError::AnnotationAuthorityMismatch);
        }
        if let Some(reference) = &self.reference
            && reference.slice.interval.contig() != self.contig
        {
            return Err(AlignmentSceneCoordinatorConfigError::ReferenceCoverageMismatch);
        }
        Ok(())
    }
}

/// Proves that a camera-local row profile still describes the exact genomic row coordinate
/// space owned by `scene` under `contract`.
///
/// Visible-row coverage is deliberately absent from this predicate. Scrolling changes which
/// geometry pages are ready to draw, but it does not change the number, identity, or activity of
/// canonical rows at the same genomic interval.
#[must_use]
pub fn alignment_row_activity_satisfies_contract(
    profile: &AlignmentRowActivityProfile,
    scene: &AlignmentSceneSnapshot,
    interval: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    let atlas = scene.reads().atlas();
    alignment_row_atlas_satisfies_contract(scene, interval, contract)
        && profile.interval() == interval
        && profile.atlas_revision() == atlas.revision()
        && profile.row_coordinate_space() == atlas.row_coordinate_space()
        && profile.stable_row_count() == atlas.row_count()
}

pub(crate) fn alignment_row_atlas_satisfies_contract(
    scene: &AlignmentSceneSnapshot,
    interval: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    let reads = scene.reads();
    let evidence = reads.evidence();
    let atlas = reads.atlas();
    scene.prepared_interval().contains_interval(interval)
        && evidence.coverage().covers(interval)
        && atlas.interval().contains_interval(interval)
        // A containing canonical atlas is intentionally reusable across compatible evidence
        // publications because its handles are source-stable. Treating provenance revision as
        // equality authority here made the committed scene renderable while permanently revoking
        // its scrollbar profile.
        && atlas.shares_stable_record_authority(evidence)
        && atlas.source_revision() == contract.source_revision
        && atlas.record_contract() == contract.record_contract
        && atlas.view_filter() == contract.view_filter_evaluation_fingerprint()
        && atlas.layout_contract() == contract.layout
}

/// User-adjustable preparation distances. The coordinator contains no hidden hard clamp; callers
/// choose these values within the shared allocation budget and measurements can later supply the
/// product defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentScenePreparationPolicy {
    pub behind_viewports: f64,
    pub ahead_viewports: f64,
    pub horizontal_trigger_viewports: f64,
    pub vertical_margin_rows: u32,
    pub vertical_trigger_rows: u32,
    /// After the visible/hot row window is published, incrementally retain the complete vertical
    /// Span/Read scene while the shared allocation ledger continues to admit it. This is a
    /// residency policy, not a request to draw offscreen rows.
    pub full_vertical_residency: bool,
}

impl AlignmentScenePreparationPolicy {
    pub fn new(
        behind_viewports: f64,
        ahead_viewports: f64,
        horizontal_trigger_viewports: f64,
        vertical_margin_rows: u32,
        vertical_trigger_rows: u32,
    ) -> Result<Self, AlignmentSceneCoordinatorConfigError> {
        let policy = Self {
            behind_viewports,
            ahead_viewports,
            horizontal_trigger_viewports,
            vertical_margin_rows,
            vertical_trigger_rows,
            full_vertical_residency: false,
        };
        policy.validate()?;
        Ok(policy)
    }

    #[must_use]
    pub const fn with_full_vertical_residency(mut self, enabled: bool) -> Self {
        self.full_vertical_residency = enabled;
        self
    }

    pub(crate) fn validate(self) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        if !self.behind_viewports.is_finite()
            || self.behind_viewports < 0.0
            || !self.ahead_viewports.is_finite()
            || self.ahead_viewports < 0.0
            || !self.horizontal_trigger_viewports.is_finite()
            || self.horizontal_trigger_viewports < 0.0
        {
            return Err(AlignmentSceneCoordinatorConfigError::InvalidPreparationDistance);
        }
        Ok(())
    }
}
