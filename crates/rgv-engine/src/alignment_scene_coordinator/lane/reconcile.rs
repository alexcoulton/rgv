use std::sync::Arc;

use rgv_alignment_scene::{
    AlignmentPresentationStore, AlignmentSceneSnapshot, PresentationContract, ReservationKind,
    SceneRevision,
};

use crate::{AlignmentSourceActor, CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    AlignmentSceneCoordinatorConfigError, AlignmentSceneCoordinatorError, AlignmentSceneSourceLane,
    DesiredScene, desired_rows, presentation_owner,
};

impl AlignmentSceneSourceLane {
    pub(crate) fn prewarm_presentation<B>(
        &mut self,
        owner: SourceOwnerId,
        presentation_store: &mut AlignmentPresentationStore,
        presentation: PresentationContract,
    ) -> Result<bool, AlignmentSceneCoordinatorError<B>> {
        if presentation.annotation
            != self
                .contract
                .annotation
                .as_ref()
                .map(|annotation| annotation.revision())
        {
            return Err(AlignmentSceneCoordinatorConfigError::AnnotationAuthorityMismatch.into());
        }
        let Some(desired) = self.desired.clone() else {
            return Err(AlignmentSceneCoordinatorError::MissingCamera);
        };
        let reference = match presentation.reference {
            None => None,
            Some(revision) => {
                if self.contract.presentation.reference != Some(revision) {
                    return Err(
                        AlignmentSceneCoordinatorConfigError::ReferenceAuthorityMismatch.into(),
                    );
                }
                let Some(reference) = self.matching_reference_slice(desired.source_horizon) else {
                    return Ok(false);
                };
                Some(reference)
            }
        };
        let Some(current) = presentation_store
            .prepared_for(presentation_owner(owner))
            .cloned()
        else {
            return Ok(false);
        };
        if !self.scene_roots_satisfy_contract(&current)
            || !current
                .reads()
                .evidence()
                .coverage()
                .covers(desired.source_horizon)
            || !current
                .summaries()
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            || !current
                .reads()
                .atlas()
                .interval()
                .contains_interval(desired.source_horizon)
        {
            return Ok(false);
        }
        let rows = desired_rows(
            &desired,
            current.reads().atlas().row_count(),
            self.policy.vertical_margin_rows,
        )?;
        let candidate = presentation_store.prepare_with_annotation(
            current.reads().evidence(),
            current.summaries(),
            current.reads().atlas(),
            presentation,
            desired.prepared_interval,
            rows,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Normal,
        )?;
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        drop(candidate);
        Ok(true)
    }

    /// The single statement of "this scene's immutable evidence, summary, and row roots were
    /// built under this lane's current contracts". Every reuse decision (frame completeness,
    /// coverage, retention, row extension, residency, prewarm) asks this one function, so a
    /// payload, filter, summary, or layout change cannot be honoured by one and missed by another.
    /// Presentation and coverage are compared separately because their rules differ per caller.
    pub(crate) fn scene_roots_satisfy_contract(&self, scene: &AlignmentSceneSnapshot) -> bool {
        let reads = scene.reads();
        reads.evidence().payload().satisfies(&self.contract.payload)
            && reads.evidence().view_filter().fingerprint()
                == self.contract.view_filter_evaluation_fingerprint()
            && scene.summaries().contract() == self.contract.summaries
            && reads.atlas().layout_contract() == self.contract.layout
    }

    /// Brings the lane's published scene up to its desired target using only what is already
    /// resident or cached; this never performs source I/O. When the lane builds in stages, the
    /// one multi-stage path (deriving new roots from cached evidence) is registered as a pending
    /// build instead of being run here, and the call returns without a publication.
    pub(crate) fn reconcile_from_resident_or_cache<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
    ) -> Result<Option<SceneRevision>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let result = self.reconcile_scene(owner, actor, presentation);
        // Whatever the outcome, a build the lane no longer wants must not keep its reservations.
        self.discard_superseded_scene_build(owner, presentation);
        result
    }

    fn reconcile_scene<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
    ) -> Result<Option<SceneRevision>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let Some(desired) = self.desired.clone() else {
            return Ok(None);
        };
        if self.prepared_covers_desired(owner, presentation) {
            return Ok(None);
        }
        if let Some(scene) = self.prepare_from_current_roots(owner, presentation, &desired)? {
            return Ok(Some(scene.revision()));
        }
        if let Some(scene) = self.prepare_from_source_roots(owner, actor, presentation, &desired)? {
            return Ok(Some(scene.revision()));
        }
        if !actor
            .store()
            .cached_covers(desired.source_horizon, &self.contract.payload)
        {
            return Ok(None);
        }
        let roots_cover = self.source_roots_cover(actor, &desired);
        if self.staged_scene_builds {
            self.request_scene_build(owner, presentation, &desired, roots_cover);
            return Ok(None);
        }
        self.run_scene_build(owner, actor, presentation, &desired, roots_cover)
    }

    pub(crate) fn prepare_from_current_roots<B>(
        &mut self,
        owner: SourceOwnerId,
        presentation: &mut AlignmentPresentationStore,
        desired: &DesiredScene,
    ) -> Result<Option<Arc<AlignmentSceneSnapshot>>, AlignmentSceneCoordinatorError<B>> {
        let Some(current) = presentation
            .prepared_for(presentation_owner(owner))
            .cloned()
        else {
            return Ok(None);
        };
        let reads = current.reads();
        let evidence = reads.evidence();
        let atlas = reads.atlas();
        if !self.scene_roots_satisfy_contract(&current)
            || !evidence.coverage().covers(desired.source_horizon)
            || !current
                .summaries()
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            || !atlas.interval().contains_interval(desired.source_horizon)
        {
            return Ok(None);
        }
        let rows = desired_rows(desired, atlas.row_count(), self.policy.vertical_margin_rows)?;
        let (presentation_contract, reference) =
            self.available_presentation_contract(desired.source_horizon);
        let candidate = presentation.prepare_with_annotation(
            evidence,
            current.summaries(),
            atlas,
            presentation_contract,
            desired.prepared_interval,
            rows,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Transition,
        )?;
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        let scene = presentation.stage_for(presentation_owner(owner), candidate);
        self.observe_scene_for_vertical_residency(&scene);
        self.refusal = None;
        self.telemetry.scene_publications = self.telemetry.scene_publications.saturating_add(1);
        Ok(Some(scene))
    }

    /// Reuses the source store's already-published immutable authority for another panel. This
    /// is what makes equal multi-panel requests share evidence, atlas identities, semantic pages,
    /// and source I/O rather than merely sharing a byte budget.
    pub(crate) fn prepare_from_source_roots<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        desired: &DesiredScene,
    ) -> Result<Option<Arc<AlignmentSceneSnapshot>>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let (Some(evidence), Some(summaries), Some(atlas)) = (
            actor.store().current().cloned(),
            actor.store().current_summaries().cloned(),
            actor.store().current_row_atlas().cloned(),
        ) else {
            return Ok(None);
        };
        if !evidence.payload().satisfies(&self.contract.payload)
            || evidence.view_filter().fingerprint()
                != self.contract.view_filter_evaluation_fingerprint()
            || !evidence.coverage().covers(desired.source_horizon)
            || summaries.contract() != self.contract.summaries
            || !summaries
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            || atlas.layout_contract() != self.contract.layout
            || !atlas.interval().contains_interval(desired.source_horizon)
        {
            return Ok(None);
        }
        let rows = desired_rows(desired, atlas.row_count(), self.policy.vertical_margin_rows)?;
        let (presentation_contract, reference) =
            self.available_presentation_contract(desired.source_horizon);
        let candidate = presentation.prepare_with_annotation(
            &evidence,
            &summaries,
            &atlas,
            presentation_contract,
            desired.prepared_interval,
            rows,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Transition,
        )?;
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        let scene = presentation.stage_for(presentation_owner(owner), candidate);
        self.observe_scene_for_vertical_residency(&scene);
        self.refusal = None;
        self.telemetry.scene_publications = self.telemetry.scene_publications.saturating_add(1);
        Ok(Some(scene))
    }

    pub(crate) fn source_roots_cover<B>(
        &self,
        actor: &AlignmentSourceActor<B>,
        desired: &DesiredScene,
    ) -> bool
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let (Some(evidence), Some(summaries), Some(atlas)) = (
            actor.store().current(),
            actor.store().current_summaries(),
            actor.store().current_row_atlas(),
        ) else {
            return false;
        };
        evidence.payload().satisfies(&self.contract.payload)
            && evidence.view_filter().fingerprint()
                == self.contract.view_filter_evaluation_fingerprint()
            && evidence.coverage().covers(desired.source_horizon)
            && summaries.contract() == self.contract.summaries
            && summaries
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            && atlas.layout_contract() == self.contract.layout
            && atlas.interval().contains_interval(desired.source_horizon)
    }

    pub(crate) fn prepared_covers_desired(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        let Some(desired) = &self.desired else {
            return false;
        };
        self.prepared_covers(owner, presentation, desired)
    }

    pub(crate) fn prepared_covers(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
        desired: &DesiredScene,
    ) -> bool {
        let Some(scene) = presentation.prepared_for(presentation_owner(owner)) else {
            return false;
        };
        let (available_presentation, _) =
            self.available_presentation_contract(desired.source_horizon);
        if scene.reads().presentation() != available_presentation
            || !self.scene_roots_satisfy_contract(scene)
            || !scene
                .reads()
                .evidence()
                .coverage()
                .covers(desired.source_horizon)
        {
            return false;
        }
        let rows = match desired_rows(
            desired,
            scene.reads().atlas().row_count(),
            self.policy.vertical_margin_rows,
        ) {
            Ok(rows) => rows,
            Err(_) => return false,
        };
        scene.covers(desired.prepared_interval, rows)
    }
}

/// Reference revisions invalidate only reference-interpreted page products. Comparing contracts
/// with that optional dependency erased lets the immutable core scene remain current while FASTA
/// work is pending, unavailable, or being upgraded.
pub(crate) fn presentation_core_matches(
    mut actual: PresentationContract,
    mut desired: PresentationContract,
) -> bool {
    actual.reference = None;
    desired.reference = None;
    actual == desired
}
