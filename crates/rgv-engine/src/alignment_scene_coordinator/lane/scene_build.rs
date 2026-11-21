//! Resumable construction of a lane's scene from cached canonical evidence.
//!
//! Filter evaluation, exact summaries, the row atlas and the semantic pages are each one stage.
//! A synchronous caller runs every stage in one call; a threaded driver runs one stage per worker
//! turn, so a newer camera, a lane removal or a budget control is observed between stages. Every
//! intermediate product is an owned candidate whose pins and reservations are released when the
//! build is dropped, which is all that abandoning a superseded build has to do.

use std::{sync::Arc, time::Instant};

use rgv_alignment_scene::{
    AlignmentPresentationStore, EvidenceCandidate, EvidenceStoreError, ExactSummaryCandidate,
    ReservationKind, RowAtlasCandidate, SceneRevision, alignment_filter_reference_horizon,
};
use rgv_core::GenomicInterval;

use crate::{AlignmentSourceActor, CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    AlignmentSceneCoordinatorError, AlignmentSceneSourceLane, DesiredScene, elapsed_nanoseconds,
    source_horizon,
};

/// Two cameras whose spans differ by less than this fraction are the same zoom level.
const SAME_ZOOM_RELATIVE_TOLERANCE: f64 = 1e-6;

/// One in-flight scene build. It owns everything it has reserved so far.
#[derive(Debug)]
pub(crate) struct SceneBuild {
    /// The exact target captured when the build started. Every stage builds for this target, so
    /// the result is the one a single uninterrupted build would have produced.
    desired: DesiredScene,
    /// The lane's contract generation at the start; any contract or reference change supersedes.
    contract_epoch: u64,
    /// The reference span this build's filters need, derived by its first stage. Later stages
    /// restore it because a camera sample between stages resets the lane's copy.
    filter_reference_horizon: BuiltFilterHorizon,
    stage: SceneBuildStage,
}

/// The reference span a build's filters need, once its first stage has derived it.
#[derive(Clone, Copy, Debug)]
enum BuiltFilterHorizon {
    /// The build has not run its canonical stage (or started from already-published roots).
    Underived,
    /// `None` when no filter needs reference bases.
    Derived(Option<GenomicInterval>),
}

#[derive(Debug)]
enum SceneBuildStage {
    /// Select canonical evidence and derive the reference horizon the filters need.
    Canonical,
    /// Evaluate the view filter over the canonical candidate.
    Filter { canonical: EvidenceCandidate },
    /// Build exact summary tiles for the filtered evidence.
    Summaries { evidence: EvidenceCandidate },
    /// Lay out the row atlas.
    Atlas {
        evidence: EvidenceCandidate,
        summaries: ExactSummaryCandidate,
    },
    /// Publish the three roots as the source's authority.
    Publish {
        evidence: EvidenceCandidate,
        summaries: ExactSummaryCandidate,
        atlas: RowAtlasCandidate,
    },
    /// Build the semantic pages over the published roots and stage the scene.
    Pages,
}

/// What one stage left behind.
#[derive(Debug)]
pub(crate) enum SceneBuildProgress {
    /// More stages remain.
    Continue(SceneBuild),
    /// The build ended without a scene: a reference-backed filter is waiting for its bases.
    AwaitingReference,
    /// The source roots are published; only the pages remain.
    RootsPublished(SceneBuild),
    Published(SceneRevision),
}

impl SceneBuild {
    pub(crate) fn new(desired: DesiredScene, contract_epoch: u64, roots_cover: bool) -> Self {
        Self {
            desired,
            contract_epoch,
            filter_reference_horizon: BuiltFilterHorizon::Underived,
            stage: if roots_cover {
                SceneBuildStage::Pages
            } else {
                SceneBuildStage::Canonical
            },
        }
    }

    #[cfg(test)]
    pub(crate) const fn stage_name(&self) -> &'static str {
        match self.stage {
            SceneBuildStage::Canonical => "canonical",
            SceneBuildStage::Filter { .. } => "filter",
            SceneBuildStage::Summaries { .. } => "summaries",
            SceneBuildStage::Atlas { .. } => "atlas",
            SceneBuildStage::Publish { .. } => "publish",
            SceneBuildStage::Pages => "pages",
        }
    }
}

impl AlignmentSceneSourceLane {
    /// Whether `build` is still worth finishing for the lane's current demand.
    ///
    /// A contract or reference change always supersedes it. A camera change supersedes it unless
    /// the build's exact target is still wanted, or the camera merely panned at the same zoom and
    /// is still inside the interval being built: finishing that build yields a scene which shows
    /// the live camera, where restarting on every pan sample would never publish at all.
    pub(crate) fn scene_build_is_current(
        &self,
        build: &SceneBuild,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        let Some(desired) = &self.desired else {
            return false;
        };
        if build.contract_epoch != self.contract_epoch
            || self.prepared_covers_desired(owner, presentation)
        {
            return false;
        }
        let same_target = build.desired.prepared_interval == desired.prepared_interval
            && build.desired.source_horizon == desired.source_horizon;
        let built_span = build.desired.camera.span();
        let same_zoom_pan = (desired.camera.span() - built_span).abs()
            <= built_span * SAME_ZOOM_RELATIVE_TOLERANCE
            && build
                .desired
                .prepared_interval
                .contains_interval(desired.camera.interval());
        same_target || same_zoom_pan
    }

    /// Drops an in-flight build which no longer serves the lane, releasing what it reserved.
    /// Returns whether a build was abandoned.
    pub(crate) fn discard_superseded_scene_build(
        &mut self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        let superseded = self
            .scene_build
            .as_ref()
            .is_some_and(|build| !self.scene_build_is_current(build, owner, presentation));
        if superseded {
            self.scene_build = None;
        }
        superseded
    }

    /// Records that the lane's cached evidence needs a scene built for `desired`. A build which
    /// still serves the lane is kept; anything else is replaced, which releases it.
    pub(crate) fn request_scene_build(
        &mut self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
        desired: &DesiredScene,
        roots_cover: bool,
    ) {
        if self
            .scene_build
            .as_ref()
            .is_some_and(|build| self.scene_build_is_current(build, owner, presentation))
        {
            return;
        }
        self.scene_build = Some(SceneBuild::new(
            desired.clone(),
            self.contract_epoch,
            roots_cover,
        ));
    }

    /// Runs every stage of a build for `desired` in one call.
    pub(crate) fn run_scene_build<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        desired: &DesiredScene,
        roots_cover: bool,
    ) -> Result<Option<SceneRevision>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let mut build = SceneBuild::new(desired.clone(), self.contract_epoch, roots_cover);
        loop {
            match self.advance_scene_build(owner, actor, presentation, build)? {
                SceneBuildProgress::Continue(next) | SceneBuildProgress::RootsPublished(next) => {
                    build = next;
                }
                SceneBuildProgress::AwaitingReference => return Ok(None),
                SceneBuildProgress::Published(revision) => return Ok(Some(revision)),
            }
        }
    }

    /// Runs only the root stages for `desired` and publishes them as the source's authority.
    /// Returns `false` when a reference-backed view filter is still waiting for its bases.
    #[cfg(test)]
    pub(crate) fn publish_source_roots_from_cache<B>(
        &mut self,
        actor: &mut AlignmentSourceActor<B>,
        desired: &DesiredScene,
    ) -> Result<bool, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let mut build = SceneBuild::new(desired.clone(), self.contract_epoch, false);
        loop {
            match self.advance_scene_build_roots(actor, build)? {
                SceneBuildProgress::Continue(next) => build = next,
                SceneBuildProgress::AwaitingReference => return Ok(false),
                SceneBuildProgress::RootsPublished(_) | SceneBuildProgress::Published(_) => {
                    return Ok(true);
                }
            }
        }
    }

    /// Runs exactly one stage. The build is consumed: on an error it is dropped here, which
    /// releases every candidate it held.
    pub(crate) fn advance_scene_build<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        build: SceneBuild,
    ) -> Result<SceneBuildProgress, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        if !matches!(build.stage, SceneBuildStage::Pages) {
            return self.advance_scene_build_roots(actor, build);
        }
        let scene = self
            .prepare_from_source_roots(owner, actor, presentation, &build.desired)?
            .ok_or(AlignmentSceneCoordinatorError::IncompleteCachedHorizon(
                build.desired.source_horizon,
            ))?;
        self.adopt_built_preparation(owner, presentation, &build);
        Ok(SceneBuildProgress::Published(scene.revision()))
    }

    /// After publishing a build whose target the camera has since panned away from, decides the
    /// lane's preparation target the way a camera update arriving now would: the scene just
    /// published is retained while the camera is safely inside it, so a continuous pan does not
    /// start another build on every completion.
    fn adopt_built_preparation(
        &mut self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
        build: &SceneBuild,
    ) {
        let Some(desired) = self.desired.clone() else {
            return;
        };
        if desired.prepared_interval == build.desired.prepared_interval
            && desired.source_horizon == build.desired.source_horizon
        {
            return;
        }
        let Some((prepared_interval, prepared_rows)) = self.retained_preparation_if_safe(
            owner,
            presentation,
            desired.camera,
            desired.direction,
        ) else {
            return;
        };
        let Ok(source_horizon) = source_horizon(prepared_interval, &self.contract) else {
            return;
        };
        if source_horizon == build.desired.source_horizon
            && let BuiltFilterHorizon::Derived(filter_reference_horizon) =
                build.filter_reference_horizon
        {
            self.filter_reference_horizon = filter_reference_horizon;
        }
        self.desired = Some(DesiredScene {
            prepared_interval,
            source_horizon,
            prepared_rows,
            ..desired
        });
    }

    fn advance_scene_build_roots<B>(
        &mut self,
        actor: &mut AlignmentSourceActor<B>,
        build: SceneBuild,
    ) -> Result<SceneBuildProgress, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let SceneBuild {
            desired,
            contract_epoch,
            filter_reference_horizon,
            stage,
        } = build;
        if let BuiltFilterHorizon::Derived(filter_reference_horizon) = filter_reference_horizon {
            self.filter_reference_horizon = filter_reference_horizon;
        }
        let next = |filter_reference_horizon, stage| {
            SceneBuildProgress::Continue(SceneBuild {
                desired: desired.clone(),
                contract_epoch,
                filter_reference_horizon,
                stage,
            })
        };
        match stage {
            SceneBuildStage::Canonical => {
                let Some(canonical) = self.select_canonical_evidence(actor, &desired)? else {
                    return Ok(SceneBuildProgress::AwaitingReference);
                };
                Ok(next(
                    BuiltFilterHorizon::Derived(self.filter_reference_horizon),
                    SceneBuildStage::Filter { canonical },
                ))
            }
            SceneBuildStage::Filter { canonical } => {
                let evidence = self.evaluate_view_filter(actor, canonical, &desired)?;
                Ok(next(
                    filter_reference_horizon,
                    SceneBuildStage::Summaries { evidence },
                ))
            }
            SceneBuildStage::Summaries { evidence } => {
                let root = Arc::clone(evidence.root());
                let summaries = actor.store_mut().prepare_exact_summaries(
                    &root,
                    desired.prepared_interval,
                    self.contract.summary_tile_bases,
                    self.contract.contig_length,
                    self.contract.summaries,
                    self.contract.normalization_mapped_records,
                    ReservationKind::Transition,
                )?;
                self.telemetry.exact_summary_candidates =
                    self.telemetry.exact_summary_candidates.saturating_add(1);
                Ok(next(
                    filter_reference_horizon,
                    SceneBuildStage::Atlas {
                        evidence,
                        summaries,
                    },
                ))
            }
            SceneBuildStage::Atlas {
                evidence,
                summaries,
            } => {
                let root = Arc::clone(evidence.root());
                let atlas = actor.store_mut().prepare_row_atlas(
                    &root,
                    desired.source_horizon,
                    self.contract.layout,
                    ReservationKind::Transition,
                )?;
                self.telemetry.row_atlas_candidates =
                    self.telemetry.row_atlas_candidates.saturating_add(1);
                Ok(next(
                    filter_reference_horizon,
                    SceneBuildStage::Publish {
                        evidence,
                        summaries,
                        atlas,
                    },
                ))
            }
            SceneBuildStage::Publish {
                evidence,
                summaries,
                atlas,
            } => {
                actor
                    .store_mut()
                    .publish_complete_authority(evidence, summaries, atlas)?;
                Ok(SceneBuildProgress::RootsPublished(SceneBuild {
                    desired,
                    contract_epoch,
                    filter_reference_horizon,
                    stage: SceneBuildStage::Pages,
                }))
            }
            SceneBuildStage::Pages => Ok(SceneBuildProgress::RootsPublished(SceneBuild {
                desired,
                contract_epoch,
                filter_reference_horizon,
                stage: SceneBuildStage::Pages,
            })),
        }
    }

    /// The canonical stage: selects cached evidence for the target and derives the reference
    /// span the filters need. Returns `None` when a reference-backed view filter has to wait
    /// for bases which are not installed yet.
    fn select_canonical_evidence<B>(
        &mut self,
        actor: &mut AlignmentSourceActor<B>,
        desired: &DesiredScene,
    ) -> Result<Option<EvidenceCandidate>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let canonical = actor.store_mut().prepare(
            desired.prepared_interval,
            self.contract.payload.clone(),
            ReservationKind::Transition,
        )?;
        if !canonical.root().coverage().covers(desired.source_horizon) {
            return Err(AlignmentSceneCoordinatorError::IncompleteCachedHorizon(
                desired.source_horizon,
            ));
        }
        self.telemetry.evidence_candidates = self.telemetry.evidence_candidates.saturating_add(1);
        self.filter_reference_horizon =
            self.derive_filter_reference_horizon(&canonical, desired.source_horizon)?;
        if self.contract.view_filter.requires_reference()
            && self
                .matching_reference_slice(desired.source_horizon)
                .is_none()
        {
            return Ok(None);
        }
        Ok(Some(canonical))
    }

    /// The filter stage: derives the filtered evidence root and records what it cost.
    fn evaluate_view_filter<B>(
        &mut self,
        actor: &mut AlignmentSourceActor<B>,
        canonical: EvidenceCandidate,
        desired: &DesiredScene,
    ) -> Result<EvidenceCandidate, EvidenceStoreError>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let reference = self.matching_reference_slice(desired.source_horizon);
        let filter_started = Instant::now();
        let filter_was_evaluated = !self.contract.view_filter.is_empty();
        let filter_contract_fingerprint = self.contract.view_filter.fingerprint();
        let filter_evaluation_fingerprint = self.contract.view_filter_evaluation_fingerprint();
        let evidence = actor.store_mut().prepare_filtered(
            canonical,
            desired.source_horizon,
            &self.contract.view_filter,
            filter_evaluation_fingerprint,
            reference,
            ReservationKind::Transition,
        )?;
        if filter_was_evaluated {
            let elapsed = elapsed_nanoseconds(filter_started);
            let filter = evidence.root().view_filter();
            self.telemetry.view_filter_evaluations =
                self.telemetry.view_filter_evaluations.saturating_add(1);
            self.telemetry.view_filter_records_examined = self
                .telemetry
                .view_filter_records_examined
                .saturating_add(u64::try_from(filter.eligible_records()).unwrap_or(u64::MAX));
            self.telemetry.view_filter_records_matched = self
                .telemetry
                .view_filter_records_matched
                .saturating_add(u64::try_from(filter.matching_records()).unwrap_or(u64::MAX));
            self.telemetry.view_filter_evaluation_nanoseconds = self
                .telemetry
                .view_filter_evaluation_nanoseconds
                .saturating_add(elapsed);
            self.telemetry.last_view_filter_evaluation_nanoseconds = elapsed;
            self.telemetry.last_view_filter_contract_fingerprint =
                filter_contract_fingerprint.get();
            self.telemetry.last_view_filter_evaluation_fingerprint =
                filter_evaluation_fingerprint.get();
        }
        Ok(evidence)
    }

    /// The exact reference span the lane's reference-backed filters need over `canonical`, or
    /// `None` when no filter needs reference bases.
    fn derive_filter_reference_horizon(
        &self,
        canonical: &EvidenceCandidate,
        source_horizon: GenomicInterval,
    ) -> Result<Option<GenomicInterval>, EvidenceStoreError> {
        if !self.contract.view_filter.requires_reference()
            && !self.contract.highlight_filter.requires_reference()
        {
            return Ok(None);
        }
        let mut required = source_horizon;
        for filter in [&self.contract.view_filter, &self.contract.highlight_filter] {
            if filter.requires_reference() {
                let horizon = alignment_filter_reference_horizon(
                    canonical.root(),
                    source_horizon,
                    filter,
                    self.contract.contig_length,
                )
                .map_err(EvidenceStoreError::ViewFilter)?;
                required = GenomicInterval::new(
                    required.contig(),
                    required.start().min(horizon.start()),
                    required.end().max(horizon.end()),
                )
                .expect("the union of non-empty filter reference horizons is non-empty");
            }
        }
        Ok(Some(required))
    }
}
