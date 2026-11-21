use std::sync::Arc;

use rgv_alignment_scene::{AlignmentPresentationStore, CanonicalRowBlock};

use crate::{SourceIntentKind, SourceOwnerId};

use super::{
    AlignmentPresentationPump, AlignmentSceneCoordinatorError, AlignmentSceneSourceLane,
    ForegroundRowPresentationAuthority, ForegroundRowPresentationPause,
    ForegroundRowPresentationPauseCause, coordinator_budget_pressure, desired_rows,
    ledger_capacity_improved, presentation_core_matches, presentation_owner,
    scene_build_failure_message,
};

impl AlignmentSceneSourceLane {
    pub(crate) fn current_roots_cover_desired_except_rows(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        let Some(desired) = &self.desired else {
            return false;
        };
        let Some(current) = presentation.prepared_for(presentation_owner(owner)) else {
            return false;
        };
        let reads = current.reads();
        presentation_core_matches(reads.presentation(), self.contract.presentation)
            && self.scene_roots_satisfy_contract(current)
            && reads.evidence().coverage().covers(desired.source_horizon)
            && current
                .summaries()
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            && reads
                .atlas()
                .interval()
                .contains_interval(desired.source_horizon)
    }

    pub(crate) fn needs_foreground_row_presentation(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        self.desired
            .as_ref()
            .is_some_and(|desired| desired.camera.has_visible_rows())
            && !self.prepared_covers_desired(owner, presentation)
            && self.current_roots_cover_desired_except_rows(owner, presentation)
    }

    /// The exact inputs of the next foreground row-block build, if they can be derived.
    pub(crate) fn foreground_row_authority(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> Option<ForegroundRowPresentationAuthority> {
        let desired = self.desired.as_ref()?;
        let current = presentation.prepared_for(presentation_owner(owner))?;
        let rows = desired_rows(
            desired,
            current.reads().atlas().row_count(),
            self.policy.vertical_margin_rows,
        )
        .ok()?;
        let (presentation_contract, _) =
            self.available_presentation_contract(desired.source_horizon);
        Some(ForegroundRowPresentationAuthority {
            scene: current.revision(),
            presentation: presentation_contract,
            prepared_interval: desired.prepared_interval,
            rows,
        })
    }

    /// The pause that still applies to the lane's current foreground row authority.
    pub(crate) fn foreground_row_pause(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> Option<&ForegroundRowPresentationPause> {
        let pause = self.foreground_row_pause.as_ref()?;
        (pause.authority == self.foreground_row_authority(owner, presentation)).then_some(pause)
    }

    pub(crate) fn foreground_row_presentation_failed(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        self.foreground_row_pause(owner, presentation)
            .is_some_and(|pause| {
                matches!(pause.cause, ForegroundRowPresentationPauseCause::Failed(_))
            })
    }

    /// Whether the missing visible row block should be built on this worker turn. A paused block
    /// does no work: a terminal failure never resumes for the same authority, and a capacity
    /// refusal resumes only once the shared ledger has gained room (and, while the driver is
    /// still reclaiming, not before that reclaim has finished).
    pub(crate) fn foreground_row_presentation_runnable(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
        resume_on_ledger_progress: bool,
    ) -> bool {
        if !self.needs_foreground_row_presentation(owner, presentation) {
            return false;
        }
        match self
            .foreground_row_pause(owner, presentation)
            .map(|pause| &pause.cause)
        {
            None => true,
            Some(ForegroundRowPresentationPauseCause::Failed(_)) => false,
            Some(ForegroundRowPresentationPauseCause::Pressure(paused)) => {
                resume_on_ledger_progress
                    && paused.as_ref().is_some_and(|paused| {
                        presentation
                            .ledger()
                            .progress()
                            .is_ok_and(|now| ledger_capacity_improved(paused, &now))
                    })
            }
        }
    }

    pub(crate) fn pause_foreground_rows_for_pressure(
        &mut self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) {
        self.foreground_row_pause = Some(ForegroundRowPresentationPause {
            authority: self.foreground_row_authority(owner, presentation),
            // Taken after the refused reservation was released, so only later relief counts.
            cause: ForegroundRowPresentationPauseCause::Pressure(
                presentation.ledger().progress().ok().map(Box::new),
            ),
        });
    }

    pub(crate) fn desired_row_pages_are_fully_cached(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        let (Some(desired), Some(current)) = (
            self.desired.as_ref(),
            presentation.prepared_for(presentation_owner(owner)),
        ) else {
            return false;
        };
        let Ok(rows) = desired_rows(
            desired,
            current.reads().atlas().row_count(),
            self.policy.vertical_margin_rows,
        ) else {
            return false;
        };
        if rows.is_empty() {
            return true;
        }
        let block_rows = self.contract.row_block_rows;
        let first = rows.start / block_rows.get();
        let last = rows.end.saturating_sub(1) / block_rows.get();
        let (presentation_contract, _) =
            self.available_presentation_contract(desired.source_horizon);
        (first..=last).all(|coordinate| {
            presentation
                .read_row_block_is_fully_cached(
                    current.reads().evidence(),
                    current.reads().atlas(),
                    presentation_contract,
                    desired.prepared_interval,
                    CanonicalRowBlock::new(coordinate, block_rows),
                    self.contract.contig_length,
                    self.contract.geometry_tile_bases,
                )
                .unwrap_or(false)
        })
    }

    pub(crate) fn prepare_foreground_row_presentation_once<E>(
        &mut self,
        owner: SourceOwnerId,
        presentation: &mut AlignmentPresentationStore,
    ) -> Result<AlignmentPresentationPump, AlignmentSceneCoordinatorError<E>> {
        let desired = self
            .desired
            .clone()
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        // Captured before the attempt: a failed build leaves the current scene, and therefore
        // this authority, unchanged, which is exactly what keeps the pause in force.
        let authority = self.foreground_row_authority(owner, presentation);
        let failure = match self.prepare_from_current_roots(owner, presentation, &desired) {
            Ok(Some(scene)) => {
                self.foreground_row_pause = None;
                self.telemetry.foreground_row_block_publications = self
                    .telemetry
                    .foreground_row_block_publications
                    .saturating_add(1);
                return Ok(AlignmentPresentationPump {
                    owner,
                    published_scene: Some(scene.revision()),
                    intent: SourceIntentKind::Foreground,
                    budget_limited: false,
                    failed: false,
                    completed: true,
                    pressure: None,
                });
            }
            Ok(None) => AlignmentSceneCoordinatorError::<E>::MissingCamera,
            Err(error) => error,
        };
        if let Some(pressure) = coordinator_budget_pressure(&failure) {
            self.pause_foreground_rows_for_pressure(owner, presentation);
            return Ok(AlignmentPresentationPump {
                owner,
                published_scene: None,
                intent: SourceIntentKind::Foreground,
                budget_limited: false,
                failed: false,
                completed: false,
                pressure: Some(pressure),
            });
        }
        // A deterministic failure would otherwise be rebuilt, and reported, on every worker
        // iteration. Latch it for this exact authority and attribute it to this owner alone;
        // the message is read back through `foreground_presentation_failure`.
        self.foreground_row_pause = Some(ForegroundRowPresentationPause {
            authority,
            cause: ForegroundRowPresentationPauseCause::Failed(
                scene_build_failure_message(&failure).unwrap_or_else(|| {
                    Arc::from("the alignment source failed while preparing visible rows")
                }),
            ),
        });
        Ok(AlignmentPresentationPump {
            owner,
            published_scene: None,
            intent: SourceIntentKind::Foreground,
            budget_limited: false,
            failed: true,
            completed: false,
            pressure: None,
        })
    }
}
