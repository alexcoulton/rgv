use rgv_core::GenomicInterval;

use crate::{
    AlignmentSourceActor, AlignmentSourceActorEvent, AlignmentSourceIntentSpec,
    CanonicalAlignmentSourceBackend, SourceIntentSequence, SourceOwnerId,
};

use super::{
    AlignmentSceneBuildContract, AlignmentSceneCoordinatorError, AlignmentSceneSourceLane,
    CameraDirection,
};

impl AlignmentSceneSourceLane {
    pub(crate) fn ensure_foreground<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
    ) -> Result<Option<SourceIntentSequence>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let desired = self
            .desired
            .as_ref()
            .expect("source scheduling requires a camera");
        let Some(missing) = actor.store().cached_missing_interval(
            desired.source_horizon,
            &self.contract.payload,
            desired.direction != CameraDirection::Left,
        ) else {
            return Ok(None);
        };
        if self.foreground_pressure_refusal == Some(desired.source_horizon) {
            // Already reported: this horizon's evidence cannot fit the evidence owner's cap.
            return Ok(None);
        }
        let admission_target = desired.camera.interval();
        // The physical acquisition fills the coherent browsing horizon, while Complete-read
        // admission is evaluated only against the exact visible camera. If independently cached
        // coverage leaves only an edge missing, repeat the bounded horizon so the decoder can
        // still count the whole camera rather than making a decision from a partial interval.
        let acquisition_target = if missing.contains_interval(admission_target) {
            missing
        } else {
            desired.source_horizon
        };
        if actor.schedule().foreground_pending_for(owner)
            && actor
                .schedule()
                .desired_foreground_for(owner)
                .is_some_and(|intent| {
                    source_intent_covers(intent.spec(), acquisition_target, &self.contract)
                })
        {
            return Ok(None);
        }
        let sequence = actor.submit_foreground_for(
            owner,
            source_spec(acquisition_target, admission_target, &self.contract),
        )?;
        self.telemetry.foreground_submissions =
            self.telemetry.foreground_submissions.saturating_add(1);
        Ok(Some(sequence))
    }

    pub(crate) fn ensure_speculation<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
    ) -> Result<Option<SourceIntentSequence>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let desired = self
            .desired
            .as_ref()
            .expect("source scheduling requires a camera");
        let Some(missing) = actor.store().cached_missing_interval(
            desired.source_horizon,
            &self.contract.payload,
            desired.direction != CameraDirection::Left,
        ) else {
            return Ok(None);
        };
        if self.speculation_refusal == Some(missing) {
            return Ok(None);
        }
        if actor.schedule().speculation_pending_for(owner)
            && actor
                .schedule()
                .desired_speculation_for(owner)
                .is_some_and(|intent| source_intent_covers(intent.spec(), missing, &self.contract))
        {
            return Ok(None);
        }
        let sequence =
            actor.submit_speculation_for(owner, source_spec(missing, missing, &self.contract))?;
        self.telemetry.speculation_submissions =
            self.telemetry.speculation_submissions.saturating_add(1);
        Ok(Some(sequence))
    }

    pub(crate) fn observe_source_event(&mut self, event: &AlignmentSourceActorEvent) {
        self.telemetry.source_checkpoints = self.telemetry.source_checkpoints.saturating_add(1);
        match event {
            AlignmentSourceActorEvent::WorkStarted(_) => {
                self.telemetry.source_work_started =
                    self.telemetry.source_work_started.saturating_add(1);
            }
            AlignmentSourceActorEvent::WorkRetargeted { .. } => {
                self.telemetry.source_retargets = self.telemetry.source_retargets.saturating_add(1);
            }
            AlignmentSourceActorEvent::WorkYielded { .. } => {
                self.telemetry.source_yields = self.telemetry.source_yields.saturating_add(1);
            }
            AlignmentSourceActorEvent::AcquisitionCompleted { .. } => {
                self.telemetry.source_acquisitions =
                    self.telemetry.source_acquisitions.saturating_add(1);
            }
            AlignmentSourceActorEvent::AcquisitionRefused { .. } => {
                self.telemetry.source_refusals = self.telemetry.source_refusals.saturating_add(1);
            }
            AlignmentSourceActorEvent::ControlApplied
            | AlignmentSourceActorEvent::ChunkAdmitted { .. }
            | AlignmentSourceActorEvent::SourceWaiting { .. }
            | AlignmentSourceActorEvent::SourceProgressed { .. } => {}
        }
    }
}

pub(crate) fn source_spec(
    target: GenomicInterval,
    admission_target: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> AlignmentSourceIntentSpec {
    AlignmentSourceIntentSpec::new(
        contract.source_revision,
        contract.record_contract,
        contract.payload.clone(),
        target,
    )
    .with_admission_target(admission_target)
}

/// Whether an intent which is already pending still serves a newly required physical target.
///
/// Only the physical target decides this. The admission target (the exact camera whose read
/// population the decoder counts) is deliberately not compared: it differs on every camera
/// sample, and comparing it restarted the decoder stream once per sample over uncached data. A
/// stream which already covers the required horizon is left to finish, and admission is settled
/// for the camera that is live at that moment: a completed acquisition is checked against the
/// live camera by the frame's exact row-activity count, and a refusal counted for an older
/// camera is re-requested once (see `AlignmentSceneSourceCoordinator::pump_once`).
pub(crate) fn source_intent_covers(
    intent: &AlignmentSourceIntentSpec,
    target: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    intent.source_revision() == contract.source_revision
        && intent.record_contract() == contract.record_contract
        && intent.payload().satisfies(&contract.payload)
        && intent.target().contains_interval(target)
}
