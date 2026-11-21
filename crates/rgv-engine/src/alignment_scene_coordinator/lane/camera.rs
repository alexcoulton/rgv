use std::ops::Range;

use rgv_alignment_scene::{AlignmentPresentationStore, AlignmentSceneCamera, CanonicalGenomicTile};
use rgv_core::GenomicInterval;

use crate::{AlignmentSourceActor, CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    AlignmentCoordinatorUpdate, AlignmentSceneBuildContract, AlignmentSceneCoordinatorConfigError,
    AlignmentSceneCoordinatorError, AlignmentScenePreparationPolicy, AlignmentSceneSourceLane,
    CameraDirection, DesiredScene, ForegroundRowPresentationPause,
    ForegroundRowPresentationPauseCause, presentation_core_matches, presentation_owner,
    usize_as_u64,
};

/// A pending preparation window is kept for a newer camera only while it is at most this many
/// times longer than the window that camera would request for itself.
const MAXIMUM_PENDING_WINDOW_RATIO: u64 = 2;

impl AlignmentSceneSourceLane {
    pub(crate) fn update_camera<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        camera: AlignmentSceneCamera,
        retain_prepared_scene: bool,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        if camera.interval().contig() != self.contract.contig {
            return Err(AlignmentSceneCoordinatorError::CameraContigMismatch);
        }
        if camera.interval().end() > self.contract.contig_length {
            return Err(AlignmentSceneCoordinatorError::CameraOutsideContig {
                interval: camera.interval(),
                contig_length: self.contract.contig_length,
            });
        }
        let previous_desired = self.desired.clone();
        let previous_source_horizon = previous_desired
            .as_ref()
            .map(|desired| desired.source_horizon);
        // A new camera (and every contract or policy change, which arrives through here) is a
        // new admission decision for a pressure-paused row block and for a failed candidate.
        // A terminal row-block failure stays keyed to its exact authority instead.
        if matches!(
            self.foreground_row_pause,
            Some(ForegroundRowPresentationPause {
                cause: ForegroundRowPresentationPauseCause::Pressure(_),
                ..
            })
        ) {
            self.foreground_row_pause = None;
        }
        self.scene_build_failed = false;
        let camera_sequence = self.take_camera_sequence()?;
        let direction = camera_direction(self.previous_exact_start, camera.exact_start());
        self.previous_exact_start = Some(camera.exact_start());
        let proposed_interval = prepared_interval(camera, direction, self.policy, &self.contract)?;
        let retained = retain_prepared_scene
            .then(|| self.retained_preparation_if_safe(owner, presentation, camera, direction))
            .flatten();
        // A safe current scene may remain the live authority until its trigger is crossed. Once
        // replacement is required, however, the desired preparation target is the new bounded
        // policy window—not the union of that window with the current scene. Repeated overlapping
        // pans would otherwise turn a fixed-size buffer into a monotonically growing corridor,
        // pinning every locus traversed and eventually making an ordinary source reservation fail.
        // The current immutable scene remains published below while the bounded replacement is
        // acquired, so removing the union does not replace truthful overlap with a blank sentinel.
        let unchanged_genomic_camera = retain_prepared_scene
            && previous_desired
                .as_ref()
                .is_some_and(|previous| previous.camera.viewport() == camera.viewport());
        // The same hysteresis applies to a preparation which is still being acquired or built.
        // Without it every sample of a pan over uncached data asks for a slightly different
        // horizon and restarts the decoder stream that was about to serve it.
        let pending = (retained.is_none() && retain_prepared_scene)
            .then(|| {
                self.pending_preparation_if_safe(
                    owner,
                    actor,
                    previous_desired.as_ref(),
                    camera,
                    direction,
                    proposed_interval,
                )
            })
            .flatten();
        let (prepared_interval, prepared_rows) = retained.or(pending).unwrap_or_else(|| {
            if unchanged_genomic_camera {
                // Crossing a vertical trigger must not silently reopen the horizontal/source
                // working set. Keep the already-authoritative genomic horizon and request only a
                // replacement row block.
                previous_desired
                    .as_ref()
                    .map(|previous| (previous.prepared_interval, None))
                    .unwrap_or((proposed_interval, None))
            } else {
                (proposed_interval, None)
            }
        });
        let source_horizon = source_horizon(prepared_interval, &self.contract)?;
        if previous_source_horizon != Some(source_horizon) {
            self.vertical_residency_budget_limited = false;
            self.vertical_residency_failed = false;
            self.filter_reference_horizon = None;
            self.speculation_refusal = None;
            self.foreground_pressure_refusal = None;
        }
        if self
            .refusal
            .is_some_and(|refusal| refusal.target != camera.interval())
        {
            self.refusal = None;
        }
        self.desired = Some(DesiredScene {
            camera,
            camera_sequence,
            prepared_interval,
            source_horizon,
            prepared_rows,
            direction,
        });
        self.telemetry.camera_updates = self.telemetry.camera_updates.saturating_add(1);

        let before = presentation
            .prepared_for(presentation_owner(owner))
            .map(|scene| scene.revision());
        let row_camera_only = previous_desired.as_ref().is_some_and(|previous| {
            previous.camera.viewport() == camera.viewport()
                && previous.source_horizon == source_horizon
                && previous.prepared_interval == prepared_interval
        });
        let defer_row_presentation = row_camera_only
            && !self.prepared_covers_desired(owner, presentation)
            && self.current_roots_cover_desired_except_rows(owner, presentation)
            && !self.desired_row_pages_are_fully_cached(owner, presentation);
        let published = if defer_row_presentation {
            self.telemetry.row_camera_updates_deferred =
                self.telemetry.row_camera_updates_deferred.saturating_add(1);
            None
        } else {
            self.reconcile_from_resident_or_cache(owner, actor, presentation)?
        };
        let frame = self
            .frame(owner, presentation)
            .expect("a camera update installs desired state");
        let mut update = AlignmentCoordinatorUpdate {
            camera_sequence,
            published_scene: published,
            visible_complete: frame.visible_complete(),
            reused_current_scene: published.is_none()
                && before == frame.snapshot.as_ref().map(|scene| scene.revision()),
            ..AlignmentCoordinatorUpdate::default()
        };
        if published.is_none() && update.reused_current_scene {
            self.telemetry.camera_only_updates =
                self.telemetry.camera_only_updates.saturating_add(1);
        }

        if defer_row_presentation {
            // Vertical movement reuses immutable evidence and row identity. Do not schedule BAM
            // acquisition merely because the newest visible block is waiting for its bounded
            // semantic checkpoint.
            actor.cancel_foreground_for(owner);
            actor.cancel_speculation_for(owner);
        } else if self.prepared_covers_desired(owner, presentation) {
            actor.cancel_foreground_for(owner);
            actor.cancel_speculation_for(owner);
        } else if update.visible_complete {
            actor.cancel_foreground_for(owner);
            update.speculation_submitted = self.ensure_speculation(owner, actor)?;
        } else {
            actor.cancel_speculation_for(owner);
            update.foreground_submitted = self.ensure_foreground(owner, actor)?;
        }
        Ok(update)
    }

    pub(crate) fn replace_contract<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        other_active_horizons: &[GenomicInterval],
        contract: AlignmentSceneBuildContract,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let mut contract = contract;
        // Reference authority belongs to the source revision named by the presentation contract,
        // not to one transient filter generation. Desktop contract builders deliberately leave
        // the asynchronously installed slice empty. Preserve an already installed immutable
        // slice when the replacement still names that exact reference and contig; horizon
        // sufficiency remains checked by `matching_reference_slice` and can request a larger
        if contract.reference.is_none()
            && let Some(authority) = self.contract.reference.as_ref()
            && contract.presentation.reference == Some(authority.revision())
            && authority.slice().interval.contig() == contract.contig
        {
            contract.reference = Some(authority.clone());
        }
        contract.validate()?;
        if contract.source_revision != self.contract.source_revision {
            return Err(AlignmentSceneCoordinatorConfigError::SourceAuthorityMismatch.into());
        }
        if contract.record_contract != self.contract.record_contract {
            return Err(AlignmentSceneCoordinatorConfigError::RecordAuthorityMismatch.into());
        }
        if contract.contig != self.contract.contig
            || contract.contig_length != self.contract.contig_length
        {
            return Err(AlignmentSceneCoordinatorError::CameraContigMismatch);
        }
        let camera = self
            .desired
            .as_ref()
            .map(|desired| desired.camera)
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        let filter_contract_changed = self.contract.view_filter.fingerprint()
            != contract.view_filter.fingerprint()
            || self.contract.highlight_filter.fingerprint()
                != contract.highlight_filter.fingerprint();
        if filter_contract_changed {
            self.filter_reference_horizon = None;
        }
        self.contract = contract;
        self.contract_epoch = self.contract_epoch.wrapping_add(1);
        self.vertical_residency_budget_limited = false;
        self.vertical_residency_failed = false;
        self.foreground_row_pause = None;
        // Every contract replacement creates a new derived authority. Reusing a prepared interval
        // retained from an earlier, much wider zoom can make a fine-detail contract materialise
        // per-base summaries or reference semantics across millions of offscreen bases. Rebase
        // the derived preparation target onto the current camera policy for all contract changes;
        // canonical BAM acquisitions remain cached and `prepare` can project the bounded target
        // from them without source I/O.
        let replacement_prepared = prepared_interval(
            camera,
            CameraDirection::Stationary,
            self.policy,
            &self.contract,
        )?;
        let replacement_horizon = source_horizon(replacement_prepared, &self.contract)?;
        let mut active_horizons = Vec::with_capacity(other_active_horizons.len().saturating_add(1));
        active_horizons.extend_from_slice(other_active_horizons);
        active_horizons.push(replacement_horizon);
        let demoted = actor
            .store_mut()
            .demote_exact_summary_cache_outside(&active_horizons);
        if demoted > 0 {
            self.telemetry.return_cache_demotions =
                self.telemetry.return_cache_demotions.saturating_add(1);
            self.telemetry.return_cache_entries_demoted = self
                .telemetry
                .return_cache_entries_demoted
                .saturating_add(usize_as_u64(demoted));
        }
        self.update_camera(owner, actor, presentation, camera, false)
    }

    /// Whether `camera` can keep using `prepared` without crossing the policy's horizontal
    /// trigger in its direction of travel.
    fn camera_is_horizontally_safe(
        &self,
        prepared: GenomicInterval,
        camera: AlignmentSceneCamera,
        direction: CameraDirection,
    ) -> bool {
        let trigger = camera.span() * self.policy.horizontal_trigger_viewports;
        let left = camera.exact_start() - prepared.start() as f64;
        let right = prepared.end() as f64 - (camera.exact_start() + camera.span());
        match direction {
            CameraDirection::Left => left >= trigger || prepared.start() == 0,
            CameraDirection::Right => {
                right >= trigger || prepared.end() == self.contract.contig_length
            }
            CameraDirection::Stationary => {
                (left >= trigger || prepared.start() == 0)
                    && (right >= trigger || prepared.end() == self.contract.contig_length)
            }
        }
    }

    /// The preparation target the lane is already acquiring or building, when the new camera
    /// can keep waiting for it: the camera is inside it with the same trigger margin a published
    /// scene needs, and it is not much larger than the window this camera would ask for (so a
    /// deep zoom-in does not keep decoding a wide window nobody is looking at).
    fn pending_preparation_if_safe<B>(
        &self,
        owner: SourceOwnerId,
        actor: &AlignmentSourceActor<B>,
        previous: Option<&DesiredScene>,
        camera: AlignmentSceneCamera,
        direction: CameraDirection,
        proposed: GenomicInterval,
    ) -> Option<(GenomicInterval, Option<Range<u32>>)>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let previous = previous?;
        let in_flight =
            actor.schedule().foreground_pending_for(owner) || self.scene_build.is_some();
        let pending = previous.prepared_interval;
        (in_flight
            && pending.contains_interval(camera.interval())
            && pending.len() <= proposed.len().saturating_mul(MAXIMUM_PENDING_WINDOW_RATIO)
            && self.camera_is_horizontally_safe(pending, camera, direction))
        .then_some((pending, None))
    }

    pub(crate) fn retained_preparation_if_safe(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
        camera: AlignmentSceneCamera,
        direction: CameraDirection,
    ) -> Option<(GenomicInterval, Option<Range<u32>>)> {
        let scene = presentation.prepared_for(presentation_owner(owner))?;
        if !presentation_core_matches(scene.reads().presentation(), self.contract.presentation)
            || !self.scene_roots_satisfy_contract(scene)
            || !scene
                .prepared_interval()
                .contains_interval(camera.interval())
        {
            return None;
        }
        let prepared = scene.prepared_interval();
        if !self.camera_is_horizontally_safe(prepared, camera, direction) {
            return None;
        }

        let visible = camera.visible_rows();
        if visible.is_empty() {
            return Some((prepared, None));
        }
        let prepared_rows = scene.reads().prepared_rows();
        let total_rows = u32::try_from(scene.reads().atlas().row_count()).ok()?;
        let before_safe = visible.start.saturating_sub(prepared_rows.start)
            >= self.policy.vertical_trigger_rows
            || prepared_rows.start == 0;
        let after_safe = prepared_rows.end.saturating_sub(visible.end)
            >= self.policy.vertical_trigger_rows
            || prepared_rows.end == total_rows;
        (before_safe && after_safe).then_some((prepared, Some(prepared_rows)))
    }

    pub(crate) fn take_camera_sequence<B>(
        &mut self,
    ) -> Result<u64, AlignmentSceneCoordinatorError<B>> {
        let sequence = self.next_camera_sequence;
        self.next_camera_sequence = self
            .next_camera_sequence
            .checked_add(1)
            .ok_or(AlignmentSceneCoordinatorError::CameraSequenceOverflow)?;
        Ok(sequence)
    }
}

pub(crate) fn camera_direction(previous: Option<f64>, current: f64) -> CameraDirection {
    previous.map_or(CameraDirection::Stationary, |previous| {
        if current > previous {
            CameraDirection::Right
        } else if current < previous {
            CameraDirection::Left
        } else {
            CameraDirection::Stationary
        }
    })
}

pub(crate) fn prepared_interval(
    camera: AlignmentSceneCamera,
    direction: CameraDirection,
    policy: AlignmentScenePreparationPolicy,
    contract: &AlignmentSceneBuildContract,
) -> Result<GenomicInterval, AlignmentSceneCoordinatorConfigError> {
    let (left_viewports, right_viewports) = match direction {
        CameraDirection::Left => (policy.ahead_viewports, policy.behind_viewports),
        CameraDirection::Right => (policy.behind_viewports, policy.ahead_viewports),
        CameraDirection::Stationary => {
            let margin = policy.ahead_viewports.max(policy.behind_viewports);
            (margin, margin)
        }
    };
    let span = camera.span();
    let start = (camera.exact_start() - span * left_viewports)
        .floor()
        .max(0.0) as u64;
    let end = (camera.exact_start() + span * (1.0 + right_viewports))
        .ceil()
        .min(contract.contig_length as f64) as u64;
    Ok(GenomicInterval::bounded(
        contract.contig,
        start,
        end.max(start.saturating_add(1)),
        contract.contig_length,
    )?)
}

pub(crate) fn source_horizon(
    prepared: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> Result<GenomicInterval, AlignmentSceneCoordinatorConfigError> {
    let mut start = prepared.start();
    let mut end = prepared.end();
    for tile_bases in [contract.geometry_tile_bases, contract.summary_tile_bases] {
        let tiles = CanonicalGenomicTile::covering(prepared, tile_bases, contract.contig_length)?;
        if let Some(first) = tiles.first() {
            start = start.min(first.interval(contract.contig_length)?.start());
        }
        if let Some(last) = tiles.last() {
            end = end.max(last.interval(contract.contig_length)?.end());
        }
    }
    Ok(GenomicInterval::new(contract.contig, start, end)?)
}

pub(crate) fn prepared_rows(
    row_count: usize,
    camera: AlignmentSceneCamera,
    margin: u32,
) -> Result<Range<u32>, AlignmentSceneCoordinatorConfigError> {
    let row_count = u32::try_from(row_count)
        .map_err(|_| AlignmentSceneCoordinatorConfigError::RowCoordinateOverflow)?;
    if row_count == 0 {
        return Ok(0..0);
    }
    let visible = clamp_visible_rows(camera.visible_rows(), row_count);
    if visible.is_empty() {
        return Ok(0..0);
    }
    Ok(visible.start.saturating_sub(margin).min(row_count)
        ..visible.end.saturating_add(margin).min(row_count))
}

pub(crate) fn desired_rows(
    desired: &DesiredScene,
    row_count: usize,
    margin: u32,
) -> Result<Range<u32>, AlignmentSceneCoordinatorConfigError> {
    if let Some(rows) = &desired.prepared_rows {
        let row_count = u32::try_from(row_count)
            .map_err(|_| AlignmentSceneCoordinatorConfigError::RowCoordinateOverflow)?;
        return Ok(clamp_visible_rows(rows.clone(), row_count));
    }
    prepared_rows(row_count, desired.camera, margin)
}

pub(crate) fn clamp_visible_rows(rows: Range<u32>, row_count: u32) -> Range<u32> {
    if rows.is_empty() || row_count == 0 {
        return 0..0;
    }
    let width = rows.end.saturating_sub(rows.start).max(1).min(row_count);
    if rows.start >= row_count || rows.end > row_count {
        return row_count.saturating_sub(width)..row_count;
    }
    rows.start..rows.end.max(rows.start.saturating_add(1)).min(row_count)
}
