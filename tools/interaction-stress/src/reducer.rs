use anyhow::bail;
use rgv_core::{
    InteractionStressScenario, InteractionStressStep, StressFailureSignature, StressNativeEvent,
    StressPredicate,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReductionReport {
    pub original_steps: usize,
    pub minimized_steps: usize,
    pub original_events: usize,
    pub minimized_events: usize,
    pub candidates_attempted: usize,
    pub candidates_preserving_signature: usize,
    pub signature: StressFailureSignature,
    pub minimized: InteractionStressScenario,
}

/// Reduces an expanded scenario using a caller-provided failure-signature evaluator.
///
/// The evaluator must run the candidate as many times as its native reproduction threshold
/// requires and return true only when the original typed signature is preserved.
pub fn reduce_with_evaluator<F>(
    source: &InteractionStressScenario,
    signature: StressFailureSignature,
    mut preserves_signature: F,
) -> anyhow::Result<ReductionReport>
where
    F: FnMut(&InteractionStressScenario) -> anyhow::Result<bool>,
{
    source.validate()?;
    if source
        .steps
        .iter()
        .any(|step| matches!(step, InteractionStressStep::Generated { .. }))
    {
        bail!("reduction requires a fully expanded scenario");
    }
    let original_steps = source.steps.len();
    let original_events = event_count(&source.steps);
    let mut minimized = source.clone();
    expand_repeats(&mut minimized.steps);
    let mut candidates_attempted = 0;
    let mut candidates_preserving_signature = 0;

    let mut granularity = 2;
    loop {
        let removable = removable_indices(&minimized.steps, signature.step_id.as_deref());
        if removable.is_empty() {
            break;
        }
        let chunk_size = removable.len().div_ceil(granularity);
        let mut changed = false;
        for chunk in removable.chunks(chunk_size.max(1)) {
            let mut candidate = minimized.clone();
            for index in chunk.iter().rev() {
                candidate.steps.remove(*index);
            }
            remove_dangling_predicates(&mut candidate.steps);
            if candidate.steps.is_empty() || candidate.validate().is_err() {
                continue;
            }
            candidates_attempted += 1;
            if preserves_signature(&candidate)? {
                minimized = candidate;
                candidates_preserving_signature += 1;
                changed = true;
                granularity = 2;
                break;
            }
        }
        if changed {
            continue;
        }
        if granularity >= removable.len() {
            break;
        }
        granularity = (granularity * 2).min(removable.len());
    }

    for step_index in 0..minimized.steps.len() {
        loop {
            let Some(events) = gesture_events(&minimized.steps[step_index]) else {
                break;
            };
            let removable = removable_event_indices(events);
            if removable.is_empty() {
                break;
            }
            let mut changed = false;
            for index in removable {
                let mut candidate = minimized.clone();
                let Some(candidate_events) = gesture_events_mut(&mut candidate.steps[step_index])
                else {
                    break;
                };
                candidate_events.remove(index);
                normalize_event_times(candidate_events);
                if candidate.validate().is_err() {
                    continue;
                }
                candidates_attempted += 1;
                if preserves_signature(&candidate)? {
                    minimized = candidate;
                    candidates_preserving_signature += 1;
                    changed = true;
                    break;
                }
            }
            if !changed {
                break;
            }
        }
    }

    Ok(ReductionReport {
        original_steps,
        minimized_steps: minimized.steps.len(),
        original_events,
        minimized_events: event_count(&minimized.steps),
        candidates_attempted,
        candidates_preserving_signature,
        signature,
        minimized,
    })
}

/// Produces a conservative offline reduction around a known failing step.
/// Native failure preservation still requires `reduce_with_evaluator` and real replay trials.
pub fn reduce_structurally(
    source: &InteractionStressScenario,
    signature: StressFailureSignature,
) -> anyhow::Result<ReductionReport> {
    let required = signature.step_id.clone();
    reduce_with_evaluator(source, signature, |candidate| {
        Ok(required.as_ref().is_none_or(|required| {
            candidate.steps.iter().any(
                |step| matches!(step, InteractionStressStep::Gesture { id, .. } if id == required),
            )
        }))
    })
}

fn removable_indices(steps: &[InteractionStressStep], required_step: Option<&str>) -> Vec<usize> {
    steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match step {
            InteractionStressStep::Gesture { id, .. } if Some(id.as_str()) == required_step => None,
            InteractionStressStep::Control { .. } | InteractionStressStep::Checkpoint { .. } => {
                None
            }
            _ => Some(index),
        })
        .collect()
}

fn remove_dangling_predicates(steps: &mut Vec<InteractionStressStep>) {
    let gesture_ids = steps
        .iter()
        .filter_map(|step| match step {
            InteractionStressStep::Gesture { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    steps.retain(|step| {
        let (InteractionStressStep::Assert { predicate }
        | InteractionStressStep::Barrier { predicate, .. }) = step
        else {
            return true;
        };
        match predicate {
            StressPredicate::GestureTerminated { step_id }
            | StressPredicate::NativeRoutingComplete { step_id } => gesture_ids.contains(step_id),
            _ => true,
        }
    });
}

fn expand_repeats(steps: &mut Vec<InteractionStressStep>) {
    let mut serial = 0_u64;
    *steps = expand_repeats_inner(std::mem::take(steps), &mut serial);
    rewrite_external_repeat_references(steps);
}

fn expand_repeats_inner(
    source: Vec<InteractionStressStep>,
    serial: &mut u64,
) -> Vec<InteractionStressStep> {
    let mut expanded = Vec::new();
    for step in source {
        match step {
            InteractionStressStep::Repeat {
                count,
                steps: nested,
            } => {
                let block = *serial;
                *serial = serial.saturating_add(1);
                for iteration in 0..count {
                    let mut copy = expand_repeats_inner(nested.clone(), serial);
                    suffix_repeat_scope(&mut copy, &format!("@repeat-{block}-{iteration}"));
                    expanded.extend(copy);
                }
            }
            other => expanded.push(other),
        }
    }
    expanded
}

fn suffix_repeat_scope(steps: &mut [InteractionStressStep], suffix: &str) {
    let ids = steps
        .iter()
        .filter_map(|step| match step {
            InteractionStressStep::Gesture { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    for step in steps {
        match step {
            InteractionStressStep::Gesture { id, .. } if ids.contains(id) => id.push_str(suffix),
            InteractionStressStep::Assert { predicate }
            | InteractionStressStep::Barrier { predicate, .. } => match predicate {
                StressPredicate::GestureTerminated { step_id }
                | StressPredicate::NativeRoutingComplete { step_id }
                    if ids.contains(step_id) =>
                {
                    step_id.push_str(suffix);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn rewrite_external_repeat_references(steps: &mut [InteractionStressStep]) {
    let exact_ids = steps
        .iter()
        .filter_map(|step| match step {
            InteractionStressStep::Gesture { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut final_expansions = std::collections::BTreeMap::new();
    for id in &exact_ids {
        if let Some((base, _)) = id.split_once("@repeat-") {
            final_expansions.insert(base.to_owned(), id.clone());
        }
    }
    for step in steps {
        let (InteractionStressStep::Assert { predicate }
        | InteractionStressStep::Barrier { predicate, .. }) = step
        else {
            continue;
        };
        let (StressPredicate::GestureTerminated { step_id }
        | StressPredicate::NativeRoutingComplete { step_id }) = predicate
        else {
            continue;
        };
        if !exact_ids.contains(step_id)
            && let Some(expanded) = final_expansions.get(step_id)
        {
            step_id.clone_from(expanded);
        }
    }
}

fn gesture_events(step: &InteractionStressStep) -> Option<&[StressNativeEvent]> {
    match step {
        InteractionStressStep::Gesture { events, .. } => Some(events),
        _ => None,
    }
}

fn gesture_events_mut(step: &mut InteractionStressStep) -> Option<&mut Vec<StressNativeEvent>> {
    match step {
        InteractionStressStep::Gesture { events, .. } => Some(events),
        _ => None,
    }
}

fn removable_event_indices(events: &[StressNativeEvent]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(
                event,
                StressNativeEvent::Move { .. }
                    | StressNativeEvent::PrimaryDrag { .. }
                    | StressNativeEvent::SecondaryDrag { .. }
            )
            .then_some(index)
        })
        .filter(|index| {
            let event = &events[*index];
            match event {
                StressNativeEvent::Move { .. } => *index != 0,
                StressNativeEvent::PrimaryDrag { .. } => {
                    events
                        .iter()
                        .filter(|event| matches!(event, StressNativeEvent::PrimaryDrag { .. }))
                        .count()
                        > 1
                }
                StressNativeEvent::SecondaryDrag { .. } => {
                    events
                        .iter()
                        .filter(|event| matches!(event, StressNativeEvent::SecondaryDrag { .. }))
                        .count()
                        > 1
                }
                _ => false,
            }
        })
        .collect()
}

fn normalize_event_times(events: &mut [StressNativeEvent]) {
    let base = events.first().map_or(0, StressNativeEvent::at_micros);
    for event in events {
        let shifted = event.at_micros().saturating_sub(base);
        match event {
            StressNativeEvent::Move { at_micros, .. }
            | StressNativeEvent::PrimaryDown { at_micros, .. }
            | StressNativeEvent::PrimaryDrag { at_micros, .. }
            | StressNativeEvent::PrimaryUp { at_micros, .. }
            | StressNativeEvent::SecondaryDown { at_micros, .. }
            | StressNativeEvent::SecondaryDrag { at_micros, .. }
            | StressNativeEvent::SecondaryUp { at_micros, .. }
            | StressNativeEvent::Scroll { at_micros, .. }
            | StressNativeEvent::KeyDown { at_micros, .. }
            | StressNativeEvent::KeyUp { at_micros, .. }
            | StressNativeEvent::Focus { at_micros, .. }
            | StressNativeEvent::ResizeWindow { at_micros, .. }
            | StressNativeEvent::Unsupported { at_micros, .. } => *at_micros = shifted,
        }
    }
}

fn event_count(steps: &[InteractionStressStep]) -> usize {
    steps
        .iter()
        .map(|step| match step {
            InteractionStressStep::Gesture { events, .. } => events.len(),
            InteractionStressStep::Repeat { count, steps } => {
                count.saturating_mul(event_count(steps))
            }
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use rgv_core::{
        INTERACTION_STRESS_SCHEMA_NAME, INTERACTION_STRESS_SCHEMA_VERSION,
        InteractionStressScenario, PanelId, StressFailureClass, StressModifiers, StressPoint,
        StressRequirements, StressRunLimits, StressTarget, StressTiming,
    };

    use super::*;

    fn gesture(id: &str) -> InteractionStressStep {
        InteractionStressStep::Gesture {
            id: id.to_owned(),
            target: StressTarget::GenomicCanvas {
                panel_id: PanelId::new(1),
            },
            fragile_target: false,
            timing: StressTiming::Recorded,
            events: vec![
                StressNativeEvent::Move {
                    at_micros: 0,
                    position: StressPoint { x: 0.1, y: 0.5 },
                    modifiers: StressModifiers::default(),
                },
                StressNativeEvent::PrimaryDown {
                    at_micros: 1,
                    modifiers: StressModifiers::default(),
                },
                StressNativeEvent::PrimaryDrag {
                    at_micros: 2,
                    position: StressPoint { x: 0.5, y: 0.5 },
                    modifiers: StressModifiers::default(),
                },
                StressNativeEvent::PrimaryDrag {
                    at_micros: 3,
                    position: StressPoint { x: 0.8, y: 0.5 },
                    modifiers: StressModifiers::default(),
                },
                StressNativeEvent::PrimaryUp {
                    at_micros: 4,
                    modifiers: StressModifiers::default(),
                },
            ],
        }
    }

    #[test]
    fn reducer_keeps_required_failing_step_and_setup() {
        let scenario = InteractionStressScenario {
            schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
            name: "failure".to_owned(),
            seed: None,
            requirements: StressRequirements::default(),
            steps: vec![
                InteractionStressStep::Control {
                    commands: vec![rgv_core::AutomationCommand::DescribeWorkspace],
                },
                gesture("noise"),
                gesture("failure"),
            ],
            limits: StressRunLimits::default(),
        };
        let signature = StressFailureSignature {
            class: StressFailureClass::SemanticMismatch,
            step_id: Some("failure".to_owned()),
            target: None,
            owner: None,
            message: "expected failure".to_owned(),
        };
        let report = reduce_structurally(&scenario, signature).unwrap();
        assert!(report.minimized_steps < report.original_steps);
        assert!(report.minimized_events < report.original_events);
        assert!(report.minimized.steps.iter().any(
            |step| matches!(step, InteractionStressStep::Gesture { id, .. } if id == "failure")
        ));
    }

    #[test]
    fn repeat_expansion_gives_every_gesture_a_unique_valid_identity() {
        let mut scenario = InteractionStressScenario {
            schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
            name: "repeated failure".to_owned(),
            seed: None,
            requirements: StressRequirements::default(),
            steps: vec![InteractionStressStep::Repeat {
                count: 3,
                steps: vec![gesture("repeat-me")],
            }],
            limits: StressRunLimits::default(),
        };
        expand_repeats(&mut scenario.steps);
        scenario.validate().unwrap();
        let ids = scenario
            .steps
            .iter()
            .filter_map(|step| match step {
                InteractionStressStep::Gesture { id, .. } => Some(id),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn event_reduction_tries_later_samples_after_a_required_sample_fails() {
        let scenario = InteractionStressScenario {
            schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
            name: "sample reduction".to_owned(),
            seed: None,
            requirements: StressRequirements::default(),
            steps: vec![gesture("failure")],
            limits: StressRunLimits::default(),
        };
        let signature = StressFailureSignature {
            class: StressFailureClass::SemanticMismatch,
            step_id: Some("failure".to_owned()),
            target: None,
            owner: None,
            message: "requires the first drag sample".to_owned(),
        };
        let report = reduce_with_evaluator(&scenario, signature, |candidate| {
            Ok(candidate.steps.iter().any(|step| {
                gesture_events(step).is_some_and(|events| {
                    events.iter().any(|event| {
                        matches!(
                            event,
                            StressNativeEvent::PrimaryDrag {
                                position: StressPoint { x: 0.5, .. },
                                ..
                            }
                        )
                    })
                })
            }))
        })
        .unwrap();
        assert_eq!(report.minimized_events, 4);
        assert_eq!(report.candidates_preserving_signature, 1);
    }
}
