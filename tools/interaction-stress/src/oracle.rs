use std::collections::BTreeMap;

use rgv_core::{
    AutomationWorkspaceSnapshot, NativeObservationSummary, StressFailureClass,
    StressFailureSignature, StressHealthSnapshot, StressPredicate,
};

#[derive(Clone, Debug)]
pub struct OracleBaseline {
    pub workspace: AutomationWorkspaceSnapshot,
    pub health: StressHealthSnapshot,
}

impl OracleBaseline {
    pub fn new(workspace: AutomationWorkspaceSnapshot, health: StressHealthSnapshot) -> Self {
        Self { workspace, health }
    }
}

#[allow(clippy::too_many_lines)]
pub fn evaluate_predicate(
    predicate: &StressPredicate,
    baseline: &OracleBaseline,
    workspace: &AutomationWorkspaceSnapshot,
    health: &StressHealthSnapshot,
    observations: &BTreeMap<String, NativeObservationSummary>,
) -> Result<(), StressFailureSignature> {
    if let Some(code) = &health.terminal_failure_code {
        return Err(failure(
            StressFailureClass::CriticalInvariant,
            None,
            format!("RGV reported terminal failure {code}"),
        ));
    }
    if health.committed_generation > health.desired_generation {
        return Err(failure(
            StressFailureClass::CriticalInvariant,
            None,
            "committed generation advanced beyond desired authority",
        ));
    }
    match predicate {
        StressPredicate::WorkspaceSettled if !workspace.settled => Err(failure(
            StressFailureClass::PipelineStall,
            None,
            "workspace has not settled",
        )),
        StressPredicate::PanelLocus { panel_id, locus } => {
            let observed = workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| panel.locus.as_str());
            if observed == Some(locus.as_str()) {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::SemanticMismatch,
                    None,
                    format!(
                        "panel {} locus mismatch: expected {locus}, observed {}",
                        panel_id.get(),
                        observed.unwrap_or("<missing>")
                    ),
                ))
            }
        }
        StressPredicate::PanelViewportChanged { panel_id } => {
            let before = baseline
                .workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| (panel.start, panel.end));
            let after = workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| (panel.start, panel.end));
            if before.is_some() && after.is_some() && before != after {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::SemanticMismatch,
                    None,
                    format!("panel {} viewport did not change", panel_id.get()),
                ))
            }
        }
        StressPredicate::ReadScrollChanged {
            panel_id,
            track_id,
            minimum_delta_points,
        } => {
            let scroll = |snapshot: &AutomationWorkspaceSnapshot| {
                snapshot
                    .panels
                    .iter()
                    .find(|panel| panel.id == *panel_id)
                    .and_then(|panel| {
                        panel
                            .read_panel_states
                            .iter()
                            .find(|state| state.track_id == *track_id)
                    })
                    .map(|state| state.scroll_points)
            };
            let before = scroll(&baseline.workspace);
            let after = scroll(workspace);
            if before
                .zip(after)
                .is_some_and(|(before, after)| (after - before).abs() >= *minimum_delta_points)
            {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::SemanticMismatch,
                    None,
                    format!(
                        "read scroll did not change by {minimum_delta_points} points for panel {} track {}",
                        panel_id.get(),
                        track_id.get()
                    ),
                ))
            }
        }
        StressPredicate::GestureTerminated { step_id } => {
            let observation = observations.get(step_id);
            if observation.is_some_and(|observation| observation.terminated)
                && health.active_interaction_id.is_none()
            {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::InteractionLock,
                    Some(step_id.clone()),
                    "gesture did not release or cancel its interaction",
                ))
            }
        }
        StressPredicate::NativeRoutingComplete { step_id } => {
            let observation = observations.get(step_id);
            if observation.is_some_and(|observation| observation.extraneous_events > 0) {
                return Err(failure(
                    StressFailureClass::ExtraneousNativeInput,
                    Some(step_id.clone()),
                    "another interaction lifecycle overlapped the expected native stream",
                ));
            }
            if observation.is_some_and(|observation| {
                observation.received_events > 0
                    && observation.applied_events > 0
                    && observation.target_acquired
            }) {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::NativeEventMissing,
                    Some(step_id.clone()),
                    "RGV did not confirm receipt and application of the native gesture",
                ))
            }
        }
        StressPredicate::UiHeartbeatAdvanced { minimum_advance } => {
            if health.ui_heartbeat_sequence
                >= baseline
                    .health
                    .ui_heartbeat_sequence
                    .saturating_add(*minimum_advance)
            {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::UiStall,
                    None,
                    "UI heartbeat did not advance by the required amount",
                ))
            }
        }
        StressPredicate::PipelineProgressAdvanced { minimum_advance } => {
            if health.worker_progress_sequence
                >= baseline
                    .health
                    .worker_progress_sequence
                    .saturating_add(*minimum_advance)
            {
                Ok(())
            } else {
                Err(failure(
                    StressFailureClass::PipelineStall,
                    None,
                    "pipeline progress did not advance by the required amount",
                ))
            }
        }
        StressPredicate::NoCriticalFailure | StressPredicate::WorkspaceSettled => Ok(()),
    }
}

fn failure(
    class: StressFailureClass,
    step_id: Option<String>,
    message: impl Into<String>,
) -> StressFailureSignature {
    StressFailureSignature {
        class,
        step_id,
        target: None,
        owner: None,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(settled: bool, start: u64) -> AutomationWorkspaceSnapshot {
        serde_json::from_str(&format!(
            r#"{{"application_version":"test","schema_version":6,"application_instance_id":"a","workspace_revision":1,"genome":{{"name":"test","reference_configured":false,"reference_ready":false}},"settled":{settled},"active_panel_id":1,"panels":[{{"id":1,"locus":"chr1:{}-100","contig":"chr1","start":{start},"end":100,"detail_level":"Bases","vertical_scroll_points":0.0,"read_panel_states":[]}}],"tracks":[],"status":[],"truncated":false}}"#,
            start + 1
        ))
        .unwrap()
    }

    fn health(heartbeat: u64) -> StressHealthSnapshot {
        StressHealthSnapshot {
            schema_version: 1,
            application_instance_id: "a".to_owned(),
            process_id: 1,
            ui_heartbeat_sequence: heartbeat,
            desired_generation: 1,
            committed_generation: 1,
            ..StressHealthSnapshot::default()
        }
    }

    #[test]
    fn semantic_and_liveness_predicates_use_explicit_baselines() {
        let baseline = OracleBaseline::new(workspace(true, 0), health(4));
        let after = workspace(true, 10);
        assert!(
            evaluate_predicate(
                &StressPredicate::PanelViewportChanged {
                    panel_id: rgv_core::PanelId::new(1)
                },
                &baseline,
                &after,
                &health(5),
                &BTreeMap::new()
            )
            .is_ok()
        );
        assert!(
            evaluate_predicate(
                &StressPredicate::UiHeartbeatAdvanced { minimum_advance: 2 },
                &baseline,
                &after,
                &health(5),
                &BTreeMap::new()
            )
            .is_err()
        );
    }
}
