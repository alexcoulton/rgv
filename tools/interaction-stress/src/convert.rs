use std::{
    io::{BufRead as _, BufReader},
    path::Path,
};

use anyhow::{Context as _, bail};
use rgv_automation::{
    AutomationRectSnapshot, INTERACTION_RECORDING_SCHEMA_NAME,
    INTERACTION_RECORDING_SCHEMA_VERSION, INTERACTION_STRESS_SCHEMA_NAME,
    INTERACTION_STRESS_SCHEMA_VERSION, InteractionLifecycle, InteractionRecordingEntry,
    InteractionRecordingHeader, InteractionStressScenario, InteractionStressStep,
    MAXIMUM_STRESS_NATIVE_EVENTS, NativeInputObservation, StressNativeEvent, StressPoint,
    StressRequirements, StressRunLimits, StressTarget, StressTiming,
};
use serde::{Deserialize, Serialize};

const MAXIMUM_RECORDING_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_RECORDING_BYTES: u64 = 256 * 1024 * 1024;
const MAXIMUM_PENDING_MOVE_MICROS: u64 = 500_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionReport {
    pub source_recording_id: String,
    pub complete_source: bool,
    pub input_events: u64,
    pub converted_gestures: usize,
    pub fragile_gestures: usize,
    pub unsupported_events: usize,
    pub warnings: Vec<String>,
    pub scenario: InteractionStressScenario,
}

struct GestureBuilder {
    id: String,
    target: StressTarget,
    fragile_target: bool,
    target_rect: Option<AutomationRectSnapshot>,
    base_micros: u64,
    events: Vec<StressNativeEvent>,
    phase_less_scroll: bool,
}

#[allow(clippy::too_many_lines)]
pub fn convert_recording(
    path: impl AsRef<Path>,
    allow_incomplete: bool,
) -> anyhow::Result<ConversionReport> {
    let path = path.as_ref();
    if std::fs::metadata(path)?.len() > MAXIMUM_RECORDING_BYTES {
        bail!("recording exceeds the {MAXIMUM_RECORDING_BYTES}-byte input limit");
    }
    let file = std::fs::File::open(path)
        .with_context(|| format!("could not open interaction recording {}", path.display()))?;
    let mut entries = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("could not read recording line {}", index + 1))?;
        if line.len() > MAXIMUM_RECORDING_LINE_BYTES {
            bail!(
                "recording line {} exceeds the bounded line limit",
                index + 1
            );
        }
        let entry = serde_json::from_str::<InteractionRecordingEntry>(&line)
            .with_context(|| format!("could not parse recording line {}", index + 1))?;
        entries.push(entry);
        if entries.len() > MAXIMUM_STRESS_NATIVE_EVENTS.saturating_add(10_000) {
            bail!("recording exceeds the bounded entry limit");
        }
    }
    let Some(InteractionRecordingEntry::Header(header)) = entries.first() else {
        bail!("recording does not begin with a header");
    };
    validate_header(header)?;
    let footer = entries.iter().rev().find_map(|entry| match entry {
        InteractionRecordingEntry::Footer(footer) => Some(footer),
        _ => None,
    });
    let complete_source = footer.is_some_and(|footer| footer.complete);
    if !complete_source && !allow_incomplete {
        bail!("recording is incomplete; pass --allow-incomplete only for forensic conversion");
    }
    validate_sequence_order(&entries)?;

    let mut steps = Vec::new();
    let mut warnings = Vec::new();
    let mut unsupported_events = 0;
    let mut fragile_gestures = 0;
    let mut gesture_index = 0_usize;
    let mut current: Option<GestureBuilder> = None;
    let mut pending_move: Option<NativeInputObservation> = None;

    for entry in &entries[1..] {
        match entry {
            InteractionRecordingEntry::Input(observation) => {
                if let StressNativeEvent::Unsupported { class, reason, .. } = &observation.event {
                    finish_gesture(
                        &mut current,
                        &mut steps,
                        &mut fragile_gestures,
                        &mut warnings,
                    );
                    unsupported_events += 1;
                    pending_move = None;
                    warnings.push(format!(
                        "sequence {} ({class}) was not converted: {reason}",
                        observation.sequence
                    ));
                    continue;
                }
                match observation.lifecycle {
                    InteractionLifecycle::Began => {
                        finish_gesture(
                            &mut current,
                            &mut steps,
                            &mut fragile_gestures,
                            &mut warnings,
                        );
                        gesture_index += 1;
                        let target = observation
                            .target
                            .clone()
                            .unwrap_or_else(|| fallback_target(header));
                        let fragile_target = is_fragile(&target) || observation.target.is_none();
                        let include_move = pending_move.take().filter(|movement| {
                            movement.target.as_ref() == Some(&target)
                                && observation
                                    .elapsed_micros
                                    .saturating_sub(movement.elapsed_micros)
                                    <= MAXIMUM_PENDING_MOVE_MICROS
                        });
                        let base_micros = include_move
                            .as_ref()
                            .map_or(observation.elapsed_micros, |movement| {
                                movement.elapsed_micros
                            });
                        let mut builder = GestureBuilder {
                            id: format!("recorded-{gesture_index}"),
                            target,
                            fragile_target,
                            base_micros,
                            events: Vec::new(),
                            target_rect: observation.target_rect,
                            phase_less_scroll: false,
                        };
                        if let Some(movement) = include_move {
                            append_observation(&mut builder, &movement, &mut warnings);
                        }
                        append_observation(&mut builder, observation, &mut warnings);
                        current = Some(builder);
                    }
                    InteractionLifecycle::Advanced => {
                        if current.is_none() {
                            gesture_index += 1;
                            let target = observation
                                .target
                                .clone()
                                .unwrap_or_else(|| fallback_target(header));
                            current = Some(GestureBuilder {
                                id: format!("recorded-{gesture_index}"),
                                fragile_target: is_fragile(&target) || observation.target.is_none(),
                                target,
                                base_micros: observation.elapsed_micros,
                                events: Vec::new(),
                                target_rect: observation.target_rect,
                                phase_less_scroll: false,
                            });
                        }
                        if let Some(builder) = current.as_mut() {
                            append_observation(builder, observation, &mut warnings);
                        }
                        if matches!(
                            observation.event,
                            StressNativeEvent::Scroll {
                                phase: rgv_automation::StressTouchPhase::Ended
                                    | rgv_automation::StressTouchPhase::Cancelled,
                                ..
                            }
                        ) {
                            finish_gesture(
                                &mut current,
                                &mut steps,
                                &mut fragile_gestures,
                                &mut warnings,
                            );
                        }
                    }
                    InteractionLifecycle::Released | InteractionLifecycle::Cancelled => {
                        if let Some(builder) = current.as_mut() {
                            append_observation(builder, observation, &mut warnings);
                        } else {
                            warnings.push(format!(
                                "sequence {} terminates an interaction with no recorded begin",
                                observation.sequence
                            ));
                        }
                        finish_gesture(
                            &mut current,
                            &mut steps,
                            &mut fragile_gestures,
                            &mut warnings,
                        );
                    }
                    InteractionLifecycle::None => match observation.event {
                        StressNativeEvent::Move { .. } => {
                            if current
                                .as_ref()
                                .is_some_and(|builder| builder.phase_less_scroll)
                            {
                                finish_gesture(
                                    &mut current,
                                    &mut steps,
                                    &mut fragile_gestures,
                                    &mut warnings,
                                );
                            }
                            pending_move = Some(observation.clone());
                        }
                        StressNativeEvent::Scroll {
                            phase:
                                rgv_automation::StressTouchPhase::None
                                | rgv_automation::StressTouchPhase::MayBegin,
                            ..
                        } => {
                            let target = observation
                                .target
                                .clone()
                                .unwrap_or_else(|| fallback_target(header));
                            let append_to_current = current.as_ref().is_some_and(|builder| {
                                builder.phase_less_scroll && builder.target == target
                            });
                            if !append_to_current {
                                finish_gesture(
                                    &mut current,
                                    &mut steps,
                                    &mut fragile_gestures,
                                    &mut warnings,
                                );
                                gesture_index += 1;
                                let include_move = pending_move.take().filter(|movement| {
                                    movement.target.as_ref() == Some(&target)
                                        && observation
                                            .elapsed_micros
                                            .saturating_sub(movement.elapsed_micros)
                                            <= MAXIMUM_PENDING_MOVE_MICROS
                                });
                                let base_micros = include_move
                                    .as_ref()
                                    .map_or(observation.elapsed_micros, |movement| {
                                        movement.elapsed_micros
                                    });
                                current = Some(GestureBuilder {
                                    id: format!("recorded-{gesture_index}"),
                                    fragile_target: is_fragile(&target)
                                        || observation.target.is_none(),
                                    target,
                                    target_rect: observation.target_rect,
                                    base_micros,
                                    events: Vec::new(),
                                    phase_less_scroll: true,
                                });
                                if let (Some(builder), Some(movement)) =
                                    (current.as_mut(), include_move)
                                {
                                    append_observation(builder, &movement, &mut warnings);
                                }
                            }
                            if let Some(builder) = current.as_mut() {
                                append_observation(builder, observation, &mut warnings);
                            }
                        }
                        StressNativeEvent::Focus { .. }
                        | StressNativeEvent::ResizeWindow { .. } => {
                            pending_move = None;
                            finish_gesture(
                                &mut current,
                                &mut steps,
                                &mut fragile_gestures,
                                &mut warnings,
                            );
                            gesture_index += 1;
                            let target = StressTarget::Window;
                            steps.push(InteractionStressStep::Gesture {
                                id: format!("recorded-{gesture_index}"),
                                target,
                                fragile_target: false,
                                timing: StressTiming::Recorded,
                                events: vec![shift_event(
                                    observation.event.clone(),
                                    observation.elapsed_micros,
                                )],
                            });
                        }
                        StressNativeEvent::KeyDown { .. } => {
                            pending_move = None;
                            finish_gesture(
                                &mut current,
                                &mut steps,
                                &mut fragile_gestures,
                                &mut warnings,
                            );
                            gesture_index += 1;
                            let target = observation
                                .target
                                .clone()
                                .unwrap_or_else(|| fallback_target(header));
                            current = Some(GestureBuilder {
                                id: format!("recorded-{gesture_index}"),
                                fragile_target: is_fragile(&target),
                                target,
                                target_rect: observation.target_rect,
                                base_micros: observation.elapsed_micros,
                                events: vec![shift_event(
                                    observation.event.clone(),
                                    observation.elapsed_micros,
                                )],
                                phase_less_scroll: false,
                            });
                        }
                        StressNativeEvent::KeyUp { .. } => {
                            pending_move = None;
                            if let Some(builder) = current.as_mut() {
                                append_observation(builder, observation, &mut warnings);
                                finish_gesture(
                                    &mut current,
                                    &mut steps,
                                    &mut fragile_gestures,
                                    &mut warnings,
                                );
                            } else {
                                warnings.push(format!(
                                    "sequence {} releases a key with no recorded press",
                                    observation.sequence
                                ));
                            }
                        }
                        _ => {}
                    },
                }
            }
            InteractionRecordingEntry::Checkpoint(checkpoint) => {
                pending_move = None;
                finish_gesture(
                    &mut current,
                    &mut steps,
                    &mut fragile_gestures,
                    &mut warnings,
                );
                steps.push(InteractionStressStep::Checkpoint {
                    label: checkpoint.label.clone(),
                    capture_native_window: false,
                });
            }
            InteractionRecordingEntry::Header(_) => {
                bail!("recording contains more than one header");
            }
            InteractionRecordingEntry::Footer(_) => break,
        }
    }
    finish_gesture(
        &mut current,
        &mut steps,
        &mut fragile_gestures,
        &mut warnings,
    );
    if steps.is_empty() {
        bail!("recording contains no replayable gestures or checkpoints");
    }
    let scenario = InteractionStressScenario {
        schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
        schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
        name: format!("recording {}", header.recording_id),
        seed: None,
        requirements: StressRequirements {
            fixture: header.fixture.clone(),
            window_logical_size: Some([
                (f64::from(header.surface_size_pixels[0]) / header.scale_factor)
                    .round()
                    .max(1.0) as u32,
                (f64::from(header.surface_size_pixels[1]) / header.scale_factor)
                    .round()
                    .max(1.0) as u32,
            ]),
            scale_factor: Some(header.scale_factor),
            source_fingerprints: header
                .initial_workspace
                .as_ref()
                .map(|workspace| {
                    workspace
                        .tracks
                        .iter()
                        .filter_map(|track| track.source_fingerprint.clone())
                        .collect()
                })
                .unwrap_or_default(),
        },
        steps,
        limits: StressRunLimits::default(),
    };
    scenario.validate()?;
    Ok(ConversionReport {
        source_recording_id: header.recording_id.clone(),
        complete_source,
        input_events: footer.map_or(0, |footer| footer.input_events),
        converted_gestures: scenario
            .steps
            .iter()
            .filter(|step| matches!(step, InteractionStressStep::Gesture { .. }))
            .count(),
        fragile_gestures,
        unsupported_events,
        warnings,
        scenario,
    })
}

fn validate_header(header: &InteractionRecordingHeader) -> anyhow::Result<()> {
    if header.schema != INTERACTION_RECORDING_SCHEMA_NAME
        || header.schema_version != INTERACTION_RECORDING_SCHEMA_VERSION
        || header.recording_id.is_empty()
        || header.scale_factor <= 0.0
        || !header.scale_factor.is_finite()
    {
        bail!("recording header is invalid or unsupported");
    }
    Ok(())
}

fn validate_sequence_order(entries: &[InteractionRecordingEntry]) -> anyhow::Result<()> {
    let mut previous = 0;
    for entry in entries {
        let sequence = match entry {
            InteractionRecordingEntry::Header(_) => continue,
            InteractionRecordingEntry::Input(value) => value.sequence,
            InteractionRecordingEntry::Checkpoint(value) => value.sequence,
            InteractionRecordingEntry::Footer(value) => value.sequence,
        };
        if sequence <= previous {
            bail!("recording sequence numbers are not strictly increasing");
        }
        previous = sequence;
    }
    Ok(())
}

fn append_observation(
    builder: &mut GestureBuilder,
    observation: &NativeInputObservation,
    warnings: &mut Vec<String>,
) {
    if observation.target.as_ref() != Some(&builder.target) {
        builder.fragile_target = true;
        warnings.push(format!(
            "gesture {} changed semantic target at sequence {}",
            builder.id, observation.sequence
        ));
    }
    let mut event = observation.event.clone();
    if let (Some([x, y]), Some(rect)) = (observation.logical_position, builder.target_rect) {
        let width = f64::from(rect.right - rect.left);
        let height = f64::from(rect.bottom - rect.top);
        if width > 0.0 && height > 0.0 {
            let position = StressPoint {
                x: (x - f64::from(rect.left)) / width,
                y: (y - f64::from(rect.top)) / height,
            };
            match &mut event {
                StressNativeEvent::Move {
                    position: current, ..
                }
                | StressNativeEvent::PrimaryDrag {
                    position: current, ..
                }
                | StressNativeEvent::SecondaryDrag {
                    position: current, ..
                } => *current = position,
                _ => {}
            }
        }
    }
    builder.events.push(shift_event(event, builder.base_micros));
}

fn finish_gesture(
    current: &mut Option<GestureBuilder>,
    steps: &mut Vec<InteractionStressStep>,
    fragile_gestures: &mut usize,
    warnings: &mut Vec<String>,
) {
    let Some(builder) = current.take() else {
        return;
    };
    if builder.events.is_empty() {
        return;
    }
    if builder.fragile_target {
        *fragile_gestures += 1;
        warnings.push(format!("gesture {} uses a fragile target", builder.id));
    }
    steps.push(InteractionStressStep::Gesture {
        id: builder.id,
        target: builder.target,
        fragile_target: builder.fragile_target,
        timing: StressTiming::Recorded,
        events: builder.events,
    });
}

fn fallback_target(header: &InteractionRecordingHeader) -> StressTarget {
    StressTarget::WindowCoordinate {
        required_logical_size: [
            (f64::from(header.surface_size_pixels[0]) / header.scale_factor)
                .round()
                .max(1.0) as u32,
            (f64::from(header.surface_size_pixels[1]) / header.scale_factor)
                .round()
                .max(1.0) as u32,
        ],
    }
}

const fn is_fragile(target: &StressTarget) -> bool {
    matches!(target, StressTarget::WindowCoordinate { .. })
}

fn shift_event(mut event: StressNativeEvent, base_micros: u64) -> StressNativeEvent {
    let shifted = event.at_micros().saturating_sub(base_micros);
    match &mut event {
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
    event
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use rgv_automation::{
        InteractionRecordingFooter, InteractionRecordingState, StressModifiers, StressPoint,
    };

    use super::*;

    fn write_recording(path: &Path, entries: Vec<InteractionRecordingEntry>) {
        let mut file = std::fs::File::create(path).unwrap();
        for entry in entries {
            serde_json::to_writer(&mut file, &entry).unwrap();
            writeln!(file).unwrap();
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn complete_pointer_recording_converts_to_target_relative_gesture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.jsonl");
        let header = InteractionRecordingHeader {
            schema: INTERACTION_RECORDING_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_RECORDING_SCHEMA_VERSION,
            recording_id: "r1".to_owned(),
            application_instance_id: "a1".to_owned(),
            process_id: 7,
            application_version: "test".to_owned(),
            started_unix_millis: 1,
            surface_size_pixels: [1280, 800],
            scale_factor: 1.0,
            fixture: Some("fixture".to_owned()),
            record_text: false,
            initial_workspace: None,
        };
        let target = StressTarget::GenomicCanvas {
            panel_id: rgv_core::PanelId::new(1),
        };
        let entries = vec![
            InteractionRecordingEntry::Header(header),
            InteractionRecordingEntry::Input(NativeInputObservation {
                sequence: 1,
                elapsed_micros: 100,
                step_id: None,
                event: StressNativeEvent::PrimaryDown {
                    at_micros: 100,
                    modifiers: StressModifiers::default(),
                },
                physical_position: Some([10.0, 20.0]),
                logical_position: Some([10.0, 20.0]),
                target: Some(target.clone()),
                target_rect: Some(AutomationRectSnapshot {
                    left: 0.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 100.0,
                }),
                layout_revision: 1,
                egui_consumed: false,
                viewer_accepted: true,
                interaction_id: Some(1),
                lifecycle: InteractionLifecycle::Began,
                active_panel_id: Some(rgv_core::PanelId::new(1)),
                panel_interval: Some([0, 100]),
                read_scroll: None,
            }),
            InteractionRecordingEntry::Input(NativeInputObservation {
                sequence: 2,
                elapsed_micros: 200,
                step_id: None,
                event: StressNativeEvent::PrimaryDrag {
                    at_micros: 200,
                    position: StressPoint { x: 0.8, y: 0.5 },
                    modifiers: StressModifiers::default(),
                },
                physical_position: Some([20.0, 20.0]),
                logical_position: Some([20.0, 20.0]),
                target: Some(target.clone()),
                target_rect: Some(AutomationRectSnapshot {
                    left: 0.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 100.0,
                }),
                layout_revision: 1,
                egui_consumed: false,
                viewer_accepted: true,
                interaction_id: Some(1),
                lifecycle: InteractionLifecycle::Advanced,
                active_panel_id: Some(rgv_core::PanelId::new(1)),
                panel_interval: Some([10, 110]),
                read_scroll: None,
            }),
            InteractionRecordingEntry::Input(NativeInputObservation {
                sequence: 3,
                elapsed_micros: 300,
                step_id: None,
                event: StressNativeEvent::PrimaryUp {
                    at_micros: 300,
                    modifiers: StressModifiers::default(),
                },
                physical_position: Some([20.0, 20.0]),
                logical_position: Some([20.0, 20.0]),
                target: Some(target),
                target_rect: Some(AutomationRectSnapshot {
                    left: 0.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 100.0,
                }),
                layout_revision: 1,
                egui_consumed: false,
                viewer_accepted: true,
                interaction_id: Some(1),
                lifecycle: InteractionLifecycle::Released,
                active_panel_id: Some(rgv_core::PanelId::new(1)),
                panel_interval: Some([10, 110]),
                read_scroll: None,
            }),
            InteractionRecordingEntry::Footer(InteractionRecordingFooter {
                sequence: 4,
                elapsed_micros: 400,
                complete: true,
                input_events: 3,
                coalesced_events: 0,
                rejected_events: 0,
                reason: None,
            }),
        ];
        write_recording(&path, entries);
        let report = convert_recording(path, false).unwrap();
        assert!(report.complete_source);
        assert_eq!(report.converted_gestures, 1);
        assert_eq!(report.fragile_gestures, 0);
        let InteractionStressStep::Gesture { events, .. } = &report.scenario.steps[0] else {
            panic!("expected gesture");
        };
        assert_eq!(events[0].at_micros(), 0);
        assert_eq!(events[2].at_micros(), 200);
        assert!(matches!(
            events[1],
            StressNativeEvent::PrimaryDrag {
                position: StressPoint { x: 0.2, y: 0.2 },
                ..
            }
        ));
        let _ = InteractionRecordingState::Complete;
    }

    #[test]
    fn consecutive_phase_less_wheel_events_form_one_complete_gesture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.jsonl");
        let header = InteractionRecordingHeader {
            schema: INTERACTION_RECORDING_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_RECORDING_SCHEMA_VERSION,
            recording_id: "wheel".to_owned(),
            application_instance_id: "a1".to_owned(),
            process_id: 7,
            application_version: "test".to_owned(),
            started_unix_millis: 1,
            surface_size_pixels: [1280, 800],
            scale_factor: 1.0,
            fixture: None,
            record_text: false,
            initial_workspace: None,
        };
        let target = StressTarget::GenomicCanvas {
            panel_id: rgv_core::PanelId::new(1),
        };
        let scroll = |sequence, elapsed_micros, delta_y| {
            InteractionRecordingEntry::Input(NativeInputObservation {
                sequence,
                elapsed_micros,
                step_id: None,
                event: StressNativeEvent::Scroll {
                    at_micros: elapsed_micros,
                    delta_x: 0.0,
                    delta_y,
                    unit: rgv_automation::StressScrollUnit::Line,
                    phase: rgv_automation::StressTouchPhase::None,
                    momentum_phase: rgv_automation::StressTouchPhase::None,
                    continuous: false,
                    modifiers: StressModifiers::default(),
                },
                physical_position: Some([20.0, 20.0]),
                logical_position: Some([20.0, 20.0]),
                target: Some(target.clone()),
                target_rect: Some(AutomationRectSnapshot {
                    left: 0.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 100.0,
                }),
                layout_revision: 1,
                egui_consumed: false,
                viewer_accepted: true,
                interaction_id: None,
                lifecycle: InteractionLifecycle::None,
                active_panel_id: Some(rgv_core::PanelId::new(1)),
                panel_interval: Some([0, 100]),
                read_scroll: None,
            })
        };
        write_recording(
            &path,
            vec![
                InteractionRecordingEntry::Header(header),
                scroll(1, 100, 3.0),
                scroll(2, 200, -2.0),
                InteractionRecordingEntry::Footer(InteractionRecordingFooter {
                    sequence: 3,
                    elapsed_micros: 300,
                    complete: true,
                    input_events: 2,
                    coalesced_events: 0,
                    rejected_events: 0,
                    reason: None,
                }),
            ],
        );

        let report = convert_recording(path, false).unwrap();
        assert_eq!(report.converted_gestures, 1);
        let InteractionStressStep::Gesture { events, .. } = &report.scenario.steps[0] else {
            panic!("expected gesture");
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].at_micros(), 0);
        assert_eq!(events[1].at_micros(), 100);
        assert!(events.iter().all(|event| matches!(
            event,
            StressNativeEvent::Scroll {
                phase: rgv_automation::StressTouchPhase::None,
                ..
            }
        )));
    }
}
