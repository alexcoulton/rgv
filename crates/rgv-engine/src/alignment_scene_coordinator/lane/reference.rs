use rgv_alignment_scene::{AlignmentPresentationStore, AlleleSummaryDetail, PresentationContract};
use rgv_core::GenomicInterval;
use rgv_hts::ReferenceSlice;

use crate::{AlignmentSourceActor, CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    ALLELE_REFERENCE_CONTEXT_RADIUS, AlignmentReferenceAuthority, AlignmentReferenceInstallOutcome,
    AlignmentReferenceRequirement, AlignmentSceneBuildContract, AlignmentSceneCoordinatorError,
    AlignmentSceneSourceLane, DesiredScene,
};

impl AlignmentSceneSourceLane {
    pub(crate) fn install_reference_authority<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        authority: AlignmentReferenceAuthority,
    ) -> Result<AlignmentReferenceInstallOutcome, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let Some(desired) = self.desired.clone() else {
            return Ok(AlignmentReferenceInstallOutcome::default());
        };
        let required_reference = self.required_reference_horizon(desired.source_horizon);
        if self.contract.presentation.reference != Some(authority.revision)
            || authority.slice.interval.contig() != self.contract.contig
            || !authority
                .slice
                .interval
                .contains_interval(required_reference)
        {
            return Ok(AlignmentReferenceInstallOutcome::default());
        }

        self.contract.reference = Some(authority);
        self.contract_epoch = self.contract_epoch.wrapping_add(1);
        self.foreground_row_pause = None;
        self.scene_build_failed = false;
        let published_scene = self.reconcile_from_resident_or_cache(owner, actor, presentation)?;
        if self.prepared_covers_desired(owner, presentation) {
            actor.cancel_foreground_for(owner);
            actor.cancel_speculation_for(owner);
        }
        Ok(AlignmentReferenceInstallOutcome {
            installed: true,
            published_scene,
        })
    }

    pub(crate) fn reference_requirement(
        &self,
        desired: &DesiredScene,
    ) -> AlignmentReferenceRequirement {
        let Some(revision) = self.contract.presentation.reference else {
            return AlignmentReferenceRequirement::NotRequired;
        };
        let horizon = self.required_reference_horizon(desired.source_horizon);
        if self
            .matching_reference_slice(desired.source_horizon)
            .is_some()
        {
            AlignmentReferenceRequirement::Ready { revision, horizon }
        } else {
            AlignmentReferenceRequirement::Pending { revision, horizon }
        }
    }

    /// Resolves the presentation contract that can be built truthfully from authority currently
    /// resident in the lane. Only the reference revision is optional: evidence, exact summaries,
    /// row layout, annotation, and every reference-independent style choice remain unchanged.
    /// A later FASTA publication upgrades this scene through the same immutable candidate path.
    pub(crate) fn available_presentation_contract(
        &self,
        source_horizon: GenomicInterval,
    ) -> (PresentationContract, Option<&ReferenceSlice>) {
        let reference = self.matching_reference_slice(source_horizon);
        let mut presentation = self.contract.presentation;
        if reference.is_none() {
            presentation.reference = None;
        }
        (presentation, reference)
    }

    pub(crate) fn required_reference_horizon(
        &self,
        source_horizon: GenomicInterval,
    ) -> GenomicInterval {
        let ordinary = required_reference_horizon(&self.contract, source_horizon);
        let Some(filter) = self
            .filter_reference_horizon
            .filter(|interval| interval.contig() == ordinary.contig())
        else {
            return ordinary;
        };
        GenomicInterval::new(
            ordinary.contig(),
            ordinary.start().min(filter.start()),
            ordinary
                .end()
                .max(filter.end())
                .min(self.contract.contig_length),
        )
        .expect("the union of non-empty reference horizons remains non-empty")
    }

    pub(crate) fn matching_reference_slice(
        &self,
        source_horizon: GenomicInterval,
    ) -> Option<&ReferenceSlice> {
        let revision = self.contract.presentation.reference?;
        let reference = self.contract.reference.as_ref()?;
        let required = self.required_reference_horizon(source_horizon);
        (reference.revision == revision
            && reference.slice.interval.contig() == self.contract.contig
            && reference.slice.interval.contains_interval(required))
        .then_some(reference.slice.as_ref())
    }
}

pub(crate) fn required_reference_horizon(
    contract: &AlignmentSceneBuildContract,
    source_horizon: GenomicInterval,
) -> GenomicInterval {
    let radius = if contract.summaries.allele_detail != AlleleSummaryDetail::None {
        ALLELE_REFERENCE_CONTEXT_RADIUS
    } else {
        0
    };
    GenomicInterval::new(
        source_horizon.contig(),
        source_horizon.start().saturating_sub(radius),
        source_horizon
            .end()
            .saturating_add(radius)
            .min(contract.contig_length),
    )
    .expect("validated source horizon remains non-empty within its reference contig")
}
