//! Presentation admission reported back to the session: the presenter tells the session which
//! complete semantic lanes it committed, which wait on presenter capacity, and which failed, and
//! the session moves each lane's navigation transaction accordingly. Moved from the desktop's
//! GPU publication path; the desktop converts its committed-lane authorities to
//! [`AlignmentLaneAdmission`]s, and a headless presenter reports the same records.

use crate::alignment::navigation::AlignmentNavigationPhase;
use crate::alignment::track::AlignmentTrackLifecycle;
use crate::panels::Panel;
use crate::session::Session;
use rgv_alignment_scene::AlignmentSceneCamera;
use rgv_core::{PanelId, TrackId};
use rgv_engine::{AlignmentSceneRequestToken, EngineQueueError};
use std::sync::Arc;

/// One lane of a presentation candidate, as the session needs to know it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentLaneAdmission {
    pub panel_id: PanelId,
    pub track_id: TrackId,
    /// The navigation request this candidate answers.
    pub desired_request_token: AlignmentSceneRequestToken,
    /// The camera the scene source prepared it for.
    pub source_camera: AlignmentSceneCamera,
    /// Whether the semantic scene is the complete answer to that request.
    pub semantic_target_complete: bool,
}

impl<P: Panel> Session<P> {
    fn admitted_projection_mut(
        &mut self,
        admission: &AlignmentLaneAdmission,
    ) -> Option<&mut crate::alignment::track::AlignmentPanelProjection> {
        if !admission.semantic_target_complete {
            return None;
        }
        let track = self
            .alignment_tracks
            .iter_mut()
            .find(|track| track.config.id == admission.track_id)?;
        let projection = track.projection_mut(admission.panel_id);
        (projection.navigation.request_token() == Some(admission.desired_request_token)
            && projection.navigation.desired_camera() == Some(admission.source_camera))
        .then_some(projection)
    }

    /// The presenter committed these lanes together with their semantic scenes.
    pub fn commit_alignment_lane_admissions(&mut self, admissions: &[AlignmentLaneAdmission]) {
        for admission in admissions {
            let Some(projection) = self.admitted_projection_mut(admission) else {
                continue;
            };
            if !matches!(
                projection.navigation.phase(),
                AlignmentNavigationPhase::CandidateReady
                    | AlignmentNavigationPhase::WaitingForGpuPressure
            ) {
                continue;
            }
            projection.navigation.transition(
                AlignmentNavigationPhase::Committed,
                "semantic scene and native GPU derivative committed atomically",
            );
            projection.finish_checking();
        }
        self.synchronize_alignment_pending_mirrors();
        self.resume_alignment_optional_work_for_settled_panels();
    }

    /// The presenter could not admit these lanes until an exact ledger blocker clears.
    pub fn wait_alignment_lane_admissions_for_gpu(
        &mut self,
        admissions: &[AlignmentLaneAdmission],
    ) {
        for admission in admissions {
            let Some(projection) = self.admitted_projection_mut(admission) else {
                continue;
            };
            if projection.navigation.phase() == AlignmentNavigationPhase::CandidateReady {
                projection.navigation.transition(
                    AlignmentNavigationPhase::WaitingForGpuPressure,
                    "atomic GPU publication is waiting for an exact ledger blocker",
                );
            }
        }
    }

    /// The blocker these lanes waited on has cleared; the presenter rebuilds the same candidate.
    pub fn resume_alignment_lane_admissions_after_gpu_progress(
        &mut self,
        admissions: &[AlignmentLaneAdmission],
    ) {
        for admission in admissions {
            let Some(projection) = self.admitted_projection_mut(admission) else {
                continue;
            };
            if projection.navigation.phase() == AlignmentNavigationPhase::WaitingForGpuPressure {
                projection.navigation.transition(
                    AlignmentNavigationPhase::CandidateReady,
                    "GPU blocker cleared; rebuilding the same atomic candidate",
                );
            }
        }
    }

    /// The presenter cannot admit these lanes at all.
    pub fn fail_alignment_lane_admissions(
        &mut self,
        admissions: &[AlignmentLaneAdmission],
        message: &str,
    ) {
        for admission in admissions {
            let Some(projection) = self.admitted_projection_mut(admission) else {
                continue;
            };
            if matches!(
                projection.navigation.phase(),
                AlignmentNavigationPhase::CandidateReady
                    | AlignmentNavigationPhase::WaitingForGpuPressure
            ) {
                projection.settle_failed(message.to_owned());
            }
        }
        self.synchronize_alignment_pending_mirrors();
        self.resume_alignment_optional_work_for_settled_panels();
    }

    /// Every lane waiting on presenter capacity settles as budget limited: the presenter shows
    /// what fits.
    pub fn settle_alignment_lane_admissions_budget_limited(&mut self, message: &str) {
        for track in &mut self.alignment_tracks {
            for projection in track.projections.values_mut() {
                if projection.navigation.phase() == AlignmentNavigationPhase::WaitingForGpuPressure
                {
                    projection.navigation.transition(
                        AlignmentNavigationPhase::BudgetLimited,
                        Arc::<str>::from(message),
                    );
                    projection.finish_checking();
                    projection.unavailable = None;
                }
            }
        }
        self.synchronize_alignment_pending_mirrors();
        self.resume_alignment_optional_work_for_settled_panels();
    }

    /// Recomputes each panel's pending-alignment flag from its transactions.
    pub fn synchronize_alignment_pending_mirrors(&mut self) {
        let panel_ids = self
            .panels
            .iter()
            .map(|panel| panel.state().id)
            .collect::<Vec<_>>();
        for panel_id in panel_ids {
            let pending = self.alignment_tracks.iter().any(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.projection(panel_id).is_some_and(|projection| {
                        !projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
            });
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.state_mut().pending_queries.alignment = pending;
            }
        }
    }

    /// Whether every alignment track of `panel_id` has settled its current request.
    pub fn panel_alignment_queries_are_settled(&self, panel_id: PanelId) -> bool {
        self.alignment_tracks
            .iter()
            .all(|track| match track.lifecycle {
                AlignmentTrackLifecycle::Opening => false,
                AlignmentTrackLifecycle::Ready => {
                    track.projection(panel_id).is_some_and(|projection| {
                        projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
                }
                AlignmentTrackLifecycle::Failed(_) => true,
            })
    }

    /// Pauses or resumes the optional (read-ahead, warm-up) work of each lane of `panel_id`.
    pub fn set_panel_alignment_optional_work_paused(&mut self, panel_id: PanelId, paused: bool) {
        for track_index in 0..self.alignment_tracks.len() {
            let Some(owner) = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| {
                    (projection.scene_optional_work_paused != paused)
                        .then_some(projection.scene_owner)
                        .flatten()
                })
            else {
                continue;
            };
            let result = self.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| engine.try_set_optional_work_paused(owner, paused));
            match result {
                Some(Ok(())) => {
                    self.alignment_tracks[track_index]
                        .projection_mut(panel_id)
                        .scene_optional_work_paused = paused;
                }
                Some(Err(EngineQueueError::Disconnected)) => {
                    self.alignment_tracks[track_index].lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
                Some(Err(EngineQueueError::Full)) | None => {}
            }
        }
    }

    /// Resumes optional work in every panel whose alignment requests have all settled.
    pub fn resume_alignment_optional_work_for_settled_panels(&mut self) {
        let settled = self
            .panels
            .iter()
            .map(|panel| panel.state().id)
            .filter(|panel_id| self.panel_alignment_queries_are_settled(*panel_id))
            .collect::<Vec<_>>();
        for panel_id in settled {
            self.set_panel_alignment_optional_work_paused(panel_id, false);
        }
    }
}
