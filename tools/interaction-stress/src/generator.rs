use std::collections::{BTreeMap, BTreeSet};

use anyhow::bail;
use rgv_automation::{
    AutomationWorkspaceSnapshot, INTERACTION_STRESS_SCHEMA_NAME, INTERACTION_STRESS_SCHEMA_VERSION,
    InteractionStressScenario, InteractionStressStep, StressActionKind, StressGeneratorProfile,
    StressGeneratorSpecification, StressModifiers, StressNativeEvent, StressPoint, StressPredicate,
    StressRequirements, StressRunLimits, StressTarget, StressTiming, StressTouchPhase,
};
use rgv_core::PanelId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedScenario {
    pub scenario: InteractionStressScenario,
    pub action_coverage: BTreeMap<String, usize>,
    pub target_coverage: BTreeMap<String, usize>,
    pub skipped_unavailable_actions: BTreeMap<String, usize>,
}

pub fn generate_scenario(
    name: &str,
    workspace: &AutomationWorkspaceSnapshot,
    window_logical_size: [u32; 2],
    specification: &StressGeneratorSpecification,
) -> anyhow::Result<GeneratedScenario> {
    if workspace.panels.is_empty() {
        bail!("the current workspace has no panel targets");
    }
    let mut random = XorShift64::new(specification.seed);
    let available = available_actions(workspace);
    let requested = if specification.allowed_actions.is_empty() {
        default_actions(specification.profile)
    } else {
        specification.allowed_actions.clone()
    };
    let choices = requested
        .iter()
        .copied()
        .filter(|action| available.contains(action))
        .collect::<Vec<_>>();
    if choices.is_empty() {
        bail!("the current workspace has no targets for the requested generated actions");
    }

    let mut steps = Vec::new();
    let mut action_coverage = BTreeMap::new();
    let mut target_coverage = BTreeMap::new();
    let mut skipped_unavailable_actions = BTreeMap::new();
    for action in requested
        .iter()
        .filter(|action| !available.contains(action))
    {
        *skipped_unavailable_actions
            .entry(action_label(*action).to_owned())
            .or_insert(0) += 1;
    }
    let mut no_wait_actions = 0;
    for index in 0..specification.actions {
        let action = weighted_choice(&choices, specification.profile, &mut random);
        let (target, events) =
            generate_action(action, workspace, window_logical_size, &mut random)?;
        let id = format!("generated-{index:05}-{}", action_label(action));
        *action_coverage
            .entry(action_label(action).to_owned())
            .or_insert(0) += 1;
        *target_coverage.entry(target_label(&target)).or_insert(0) += 1;
        steps.push(InteractionStressStep::Gesture {
            id,
            target,
            fragile_target: false,
            timing: generated_timing(specification.profile, &mut random),
            events,
        });
        no_wait_actions += 1;
        let force_barrier = no_wait_actions >= specification.maximum_no_wait_actions;
        let probabilistic_barrier = match specification.profile {
            StressGeneratorProfile::Smoke => true,
            StressGeneratorProfile::RecordingMutation => random.chance(1, 2),
            StressGeneratorProfile::Aggressive => random.chance(1, 5),
            StressGeneratorProfile::Soak => random.chance(1, 8),
        };
        if force_barrier || probabilistic_barrier {
            steps.push(InteractionStressStep::Barrier {
                predicate: StressPredicate::WorkspaceSettled,
                timeout_millis: 120_000,
            });
            no_wait_actions = 0;
        }
    }
    steps.push(InteractionStressStep::Assert {
        predicate: StressPredicate::NoCriticalFailure,
    });
    let scenario = InteractionStressScenario {
        schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
        schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
        name: name.to_owned(),
        seed: Some(specification.seed),
        requirements: StressRequirements {
            fixture: None,
            window_logical_size: Some(window_logical_size),
            scale_factor: None,
            source_fingerprints: Vec::new(),
        },
        steps,
        limits: StressRunLimits {
            maximum_duration_millis: match specification.profile {
                StressGeneratorProfile::Soak => 24 * 60 * 60 * 1_000,
                _ => 30 * 60 * 1_000,
            },
            ..StressRunLimits::default()
        },
    };
    scenario.validate()?;
    Ok(GeneratedScenario {
        scenario,
        action_coverage,
        target_coverage,
        skipped_unavailable_actions,
    })
}

pub fn mutate_scenario(
    source: &InteractionStressScenario,
    seed: u64,
    intensity: u8,
) -> anyhow::Result<InteractionStressScenario> {
    if !(1..=10).contains(&intensity) {
        bail!("mutation intensity must be within 1..=10");
    }
    let mut mutated = source.clone();
    mutated.name = format!("{} mutation {seed}", source.name);
    mutated.seed = Some(seed);
    let mut random = XorShift64::new(seed);
    mutate_steps(&mut mutated.steps, intensity, &mut random);
    let timing_and_path_only = mutated.clone();
    mutate_structure(&mut mutated.steps, intensity, &mut random);
    if mutated.validate().is_err() {
        mutated = timing_and_path_only;
    }
    mutated.validate()?;
    Ok(mutated)
}

fn mutate_steps(steps: &mut [InteractionStressStep], intensity: u8, random: &mut XorShift64) {
    for step in steps {
        match step {
            InteractionStressStep::Gesture { timing, events, .. } => {
                if random.chance(u64::from(intensity), 12) {
                    *timing = StressTiming::SeededJitter {
                        seed: random.next(),
                        maximum_jitter_micros: 500_u64.saturating_mul(u64::from(intensity)),
                    };
                } else if random.chance(u64::from(intensity), 15) {
                    *timing = StressTiming::Scaled {
                        factor: if random.chance(1, 2) { 0.25 } else { 2.0 },
                    };
                }
                if random.chance(u64::from(intensity), 20) {
                    perturb_drag_path(events, random);
                }
            }
            InteractionStressStep::Repeat { steps, .. } => {
                mutate_steps(steps, intensity, random);
            }
            _ => {}
        }
    }
}

fn mutate_structure(
    steps: &mut Vec<InteractionStressStep>,
    intensity: u8,
    random: &mut XorShift64,
) {
    let source = std::mem::take(steps);
    for mut step in source {
        if let InteractionStressStep::Repeat { steps, .. } = &mut step {
            mutate_structure(steps, intensity, random);
        }
        if matches!(
            step,
            InteractionStressStep::Barrier {
                predicate: StressPredicate::WorkspaceSettled,
                ..
            }
        ) && random.chance(u64::from(intensity), 20)
        {
            // Removing an explicit settlement point is the bounded interleaving mutation: the
            // next action starts while the preceding source/render work may still be pending.
            continue;
        }
        if matches!(step, InteractionStressStep::Gesture { .. })
            && random.chance(u64::from(intensity), 24)
        {
            steps.push(InteractionStressStep::Repeat {
                count: 2 + random.index(2),
                steps: vec![step],
            });
        } else {
            steps.push(step);
        }
    }
}

fn perturb_drag_path(events: &mut [StressNativeEvent], random: &mut XorShift64) {
    for event in events {
        if let StressNativeEvent::PrimaryDrag { position, .. }
        | StressNativeEvent::SecondaryDrag { position, .. } = event
        {
            let jitter = (random.fraction() - 0.5) * 0.2;
            if random.chance(1, 2) {
                position.x = (position.x + jitter).clamp(-0.25, 1.25);
            } else {
                position.y = (position.y + jitter).clamp(-0.25, 1.25);
            }
        }
    }
}

fn available_actions(workspace: &AutomationWorkspaceSnapshot) -> BTreeSet<StressActionKind> {
    let mut actions = BTreeSet::from([StressActionKind::FocusCycle, StressActionKind::Resize]);
    if workspace
        .panels
        .iter()
        .any(|panel| unobstructed_canvas_y(workspace, panel.id).is_some())
    {
        actions.extend([
            StressActionKind::CanvasPan,
            StressActionKind::CanvasPanReversal,
            StressActionKind::VerticalScroll,
            StressActionKind::Zoom,
            StressActionKind::ZoomReversal,
            StressActionKind::SecondaryClick,
        ]);
    }
    let read_scroll_available = workspace.panels.iter().any(|panel| {
        panel
            .read_panel_states
            .iter()
            .any(|state| state.maximum_scroll_points > 0.0)
    });
    if read_scroll_available {
        actions.insert(StressActionKind::ReadScrollbarDrag);
        actions.insert(StressActionKind::ReadScrollbarReversal);
    }
    actions
}

fn unobstructed_canvas_y(
    workspace: &AutomationWorkspaceSnapshot,
    panel_id: PanelId,
) -> Option<f64> {
    let Some(layout) = &workspace.workspace_layout else {
        return Some(0.5);
    };
    let canvas = layout
        .panels
        .iter()
        .find(|panel| panel.panel_id == panel_id)?
        .canvas;
    let canvas_height = canvas.bottom - canvas.top;
    if !canvas_height.is_finite() || canvas_height <= 0.0 {
        return None;
    }
    let mut blockers = workspace
        .pane_layouts
        .iter()
        .filter(|pane| pane.panel_id == panel_id)
        .filter_map(|pane| {
            let top = pane.pane.top.max(canvas.top);
            let bottom = pane.pane.bottom.min(canvas.bottom);
            (bottom > top).then_some((top, bottom))
        })
        .collect::<Vec<_>>();
    blockers.sort_by(|left, right| left.0.total_cmp(&right.0));

    let mut cursor = canvas.top;
    let mut best = (canvas.top, canvas.top);
    for (top, bottom) in blockers {
        if top - cursor > best.1 - best.0 {
            best = (cursor, top);
        }
        cursor = cursor.max(bottom);
    }
    if canvas.bottom - cursor > best.1 - best.0 {
        best = (cursor, canvas.bottom);
    }
    if best.1 - best.0 < 4.0 {
        return None;
    }
    let midpoint = f64::from((best.0 + best.1) * 0.5);
    Some(((midpoint - f64::from(canvas.top)) / f64::from(canvas_height)).clamp(0.0, 1.0))
}

fn default_actions(profile: StressGeneratorProfile) -> Vec<StressActionKind> {
    let mut actions = vec![
        StressActionKind::CanvasPan,
        StressActionKind::VerticalScroll,
        StressActionKind::Zoom,
        StressActionKind::SecondaryClick,
    ];
    if profile != StressGeneratorProfile::Smoke {
        actions.extend([
            StressActionKind::CanvasPanReversal,
            StressActionKind::ReadScrollbarDrag,
            StressActionKind::ReadScrollbarReversal,
            StressActionKind::ZoomReversal,
            StressActionKind::FocusCycle,
            StressActionKind::Resize,
        ]);
    }
    actions
}

fn weighted_choice(
    choices: &[StressActionKind],
    profile: StressGeneratorProfile,
    random: &mut XorShift64,
) -> StressActionKind {
    let weights = choices
        .iter()
        .map(|action| action_weight(*action, profile))
        .collect::<Vec<_>>();
    let total = weights.iter().sum::<u64>().max(1);
    let mut selected = random.next() % total;
    for (action, weight) in choices.iter().zip(weights) {
        if selected < weight {
            return *action;
        }
        selected -= weight;
    }
    choices[0]
}

const fn action_weight(action: StressActionKind, profile: StressGeneratorProfile) -> u64 {
    match (profile, action) {
        (
            StressGeneratorProfile::Aggressive,
            StressActionKind::CanvasPanReversal
            | StressActionKind::ReadScrollbarReversal
            | StressActionKind::ZoomReversal,
        ) => 5,
        (StressGeneratorProfile::Soak, StressActionKind::FocusCycle | StressActionKind::Resize) => {
            1
        }
        (
            _,
            StressActionKind::CanvasPan | StressActionKind::VerticalScroll | StressActionKind::Zoom,
        ) => 4,
        _ => 2,
    }
}

#[allow(clippy::too_many_lines)]
fn generate_action(
    action: StressActionKind,
    workspace: &AutomationWorkspaceSnapshot,
    window_logical_size: [u32; 2],
    random: &mut XorShift64,
) -> anyhow::Result<(StressTarget, Vec<StressNativeEvent>)> {
    let canvas_panels = workspace
        .panels
        .iter()
        .filter_map(|panel| unobstructed_canvas_y(workspace, panel.id).map(|y| (panel, y)))
        .collect::<Vec<_>>();
    let (panel, canvas_y) = canvas_panels
        .get(random.index(canvas_panels.len()))
        .copied()
        .unwrap_or((&workspace.panels[random.index(workspace.panels.len())], 0.5));
    let target = StressTarget::GenomicCanvas { panel_id: panel.id };
    let modifiers = StressModifiers::default();
    let result = match action {
        StressActionKind::CanvasPan | StressActionKind::CanvasPanReversal => {
            let y = canvas_y;
            let start = StressPoint {
                x: 0.2 + random.fraction() * 0.2,
                y,
            };
            let end = StressPoint {
                x: 0.65 + random.fraction() * 0.2,
                y,
            };
            let mut events = vec![
                move_event(0, start),
                StressNativeEvent::PrimaryDown {
                    at_micros: 1_000,
                    modifiers,
                },
                drag_event(
                    60_000,
                    StressPoint {
                        x: (start.x + end.x) * 0.5,
                        y,
                    },
                ),
                drag_event(100_000, end),
            ];
            if action == StressActionKind::CanvasPanReversal {
                events.push(drag_event(
                    160_000,
                    StressPoint {
                        x: start.x + 0.05,
                        y,
                    },
                ));
                events.push(drag_event(220_000, end));
            }
            events.push(StressNativeEvent::PrimaryUp {
                at_micros: 250_000,
                modifiers,
            });
            (target, events)
        }
        StressActionKind::ReadScrollbarDrag | StressActionKind::ReadScrollbarReversal => {
            let candidates = workspace
                .panels
                .iter()
                .flat_map(|panel| {
                    panel
                        .read_panel_states
                        .iter()
                        .filter(|state| state.maximum_scroll_points > 0.0)
                        .map(|state| (panel.id, state.track_id))
                })
                .collect::<Vec<_>>();
            let Some((panel_id, track_id)) =
                candidates.get(random.index(candidates.len())).copied()
            else {
                bail!("read-scrollbar action was selected without a scrollable read panel");
            };
            let start = StressPoint { x: 0.5, y: 0.15 };
            let end = StressPoint { x: 0.5, y: 0.85 };
            let mut events = vec![
                move_event(0, start),
                StressNativeEvent::PrimaryDown {
                    at_micros: 1_000,
                    modifiers,
                },
                drag_event(6_000, end),
            ];
            if action == StressActionKind::ReadScrollbarReversal {
                events.push(drag_event(9_000, StressPoint { x: 0.5, y: 0.25 }));
                events.push(drag_event(12_000, end));
            }
            events.push(StressNativeEvent::PrimaryUp {
                at_micros: 15_000,
                modifiers,
            });
            (StressTarget::ReadScrollbar { panel_id, track_id }, events)
        }
        StressActionKind::VerticalScroll => {
            let modifiers = StressModifiers {
                shift: true,
                ..StressModifiers::default()
            };
            (
                target,
                vec![
                    move_event(
                        0,
                        StressPoint {
                            x: 0.5,
                            y: canvas_y,
                        },
                    ),
                    scroll_event(1_000, -120.0, StressTouchPhase::Began, modifiers),
                    scroll_event(4_000, -280.0, StressTouchPhase::Changed, modifiers),
                    scroll_event(8_000, 0.0, StressTouchPhase::Ended, modifiers),
                ],
            )
        }
        StressActionKind::Zoom | StressActionKind::ZoomReversal => {
            let first = if random.chance(1, 2) { 180.0 } else { -180.0 };
            let mut events = vec![
                move_event(
                    0,
                    StressPoint {
                        x: 0.5,
                        y: canvas_y,
                    },
                ),
                scroll_event(1_000, first, StressTouchPhase::Began, modifiers),
                scroll_event(5_000, first, StressTouchPhase::Changed, modifiers),
            ];
            if action == StressActionKind::ZoomReversal {
                events.push(scroll_event(
                    8_000,
                    -first,
                    StressTouchPhase::Changed,
                    modifiers,
                ));
            }
            events.push(scroll_event(
                12_000,
                0.0,
                StressTouchPhase::Ended,
                modifiers,
            ));
            (target, events)
        }
        StressActionKind::SecondaryClick => (
            target,
            vec![
                move_event(
                    0,
                    StressPoint {
                        x: 0.15 + random.fraction() * 0.7,
                        y: canvas_y,
                    },
                ),
                StressNativeEvent::SecondaryDown {
                    at_micros: 1_000,
                    modifiers,
                },
                StressNativeEvent::SecondaryUp {
                    at_micros: 3_000,
                    modifiers,
                },
            ],
        ),
        StressActionKind::FocusCycle => (
            StressTarget::Window,
            vec![
                StressNativeEvent::Focus {
                    at_micros: 0,
                    focused: false,
                },
                StressNativeEvent::Focus {
                    at_micros: 30_000,
                    focused: true,
                },
            ],
        ),
        StressActionKind::Resize => {
            let mut delta = i32::try_from(random.next() % 161).unwrap_or_default() - 80;
            if delta == 0 {
                delta = 40;
            }
            let width =
                u32::try_from((i64::from(window_logical_size[0]) + i64::from(delta)).max(640))
                    .unwrap_or(640);
            let height =
                u32::try_from((i64::from(window_logical_size[1]) - i64::from(delta / 2)).max(400))
                    .unwrap_or(400);
            (
                StressTarget::Window,
                vec![StressNativeEvent::ResizeWindow {
                    at_micros: 0,
                    logical_size: [width, height],
                }],
            )
        }
    };
    Ok(result)
}

fn move_event(at_micros: u64, position: StressPoint) -> StressNativeEvent {
    StressNativeEvent::Move {
        at_micros,
        position,
        modifiers: StressModifiers::default(),
    }
}

fn drag_event(at_micros: u64, position: StressPoint) -> StressNativeEvent {
    StressNativeEvent::PrimaryDrag {
        at_micros,
        position,
        modifiers: StressModifiers::default(),
    }
}

fn scroll_event(
    at_micros: u64,
    delta_y: f64,
    phase: StressTouchPhase,
    modifiers: StressModifiers,
) -> StressNativeEvent {
    StressNativeEvent::Scroll {
        at_micros,
        delta_x: 0.0,
        delta_y,
        unit: rgv_automation::StressScrollUnit::Pixel,
        phase,
        momentum_phase: StressTouchPhase::None,
        continuous: true,
        modifiers,
    }
}

fn generated_timing(profile: StressGeneratorProfile, random: &mut XorShift64) -> StressTiming {
    match profile {
        StressGeneratorProfile::Smoke => StressTiming::Recorded,
        StressGeneratorProfile::RecordingMutation => StressTiming::SeededJitter {
            seed: random.next(),
            maximum_jitter_micros: 1_000,
        },
        StressGeneratorProfile::Aggressive => StressTiming::MaximumRate,
        StressGeneratorProfile::Soak => {
            if random.chance(1, 3) {
                StressTiming::MaximumRate
            } else {
                StressTiming::SeededJitter {
                    seed: random.next(),
                    maximum_jitter_micros: 3_000,
                }
            }
        }
    }
}

const fn action_label(action: StressActionKind) -> &'static str {
    match action {
        StressActionKind::CanvasPan => "canvas_pan",
        StressActionKind::CanvasPanReversal => "canvas_pan_reversal",
        StressActionKind::ReadScrollbarDrag => "read_scrollbar_drag",
        StressActionKind::ReadScrollbarReversal => "read_scrollbar_reversal",
        StressActionKind::VerticalScroll => "vertical_scroll",
        StressActionKind::Zoom => "zoom",
        StressActionKind::ZoomReversal => "zoom_reversal",
        StressActionKind::SecondaryClick => "secondary_click",
        StressActionKind::FocusCycle => "focus_cycle",
        StressActionKind::Resize => "resize",
    }
}

fn target_label(target: &StressTarget) -> String {
    match target {
        StressTarget::Window => "window".to_owned(),
        StressTarget::GenomicCanvas { panel_id } => format!("panel:{}:canvas", panel_id.get()),
        StressTarget::ReadCanvas { panel_id, track_id } => {
            format!("panel:{}:track:{}:reads", panel_id.get(), track_id.get())
        }
        StressTarget::ReadScrollbar { panel_id, track_id } => {
            format!(
                "panel:{}:track:{}:scrollbar",
                panel_id.get(),
                track_id.get()
            )
        }
        StressTarget::TrackHeader { panel_id, target }
        | StressTarget::PaneHeader { panel_id, target } => {
            format!("panel:{}:{target:?}:header", panel_id.get())
        }
        StressTarget::Overview { panel_id } => format!("panel:{}:overview", panel_id.get()),
        StressTarget::Ruler { panel_id } => format!("panel:{}:ruler", panel_id.get()),
        StressTarget::PaneContent { panel_id, target } => {
            format!("panel:{}:{target:?}:content", panel_id.get())
        }
        StressTarget::Accessibility { identifier } => format!("ax:{identifier}"),
        StressTarget::WindowCoordinate { .. } => "window_coordinate".to_owned(),
    }
}

struct XorShift64(u64);

impl XorShift64 {
    const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn fraction(&mut self) -> f64 {
        (self.next() >> 11) as f64 / ((1_u64 << 53) as f64)
    }

    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        denominator > 0 && self.next() % denominator < numerator.min(denominator)
    }

    fn index(&mut self, length: usize) -> usize {
        if length == 0 {
            0
        } else {
            usize::try_from(self.next() % length as u64).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use rgv_automation::{StressGeneratorProfile, StressGeneratorSpecification};

    use super::*;

    fn workspace() -> AutomationWorkspaceSnapshot {
        serde_json::from_str(
            r#"{"application_version":"test","schema_version":6,"application_instance_id":"a","workspace_revision":1,"genome":{"name":"test","reference_configured":false,"reference_ready":false},"settled":true,"active_panel_id":1,"panels":[{"id":1,"locus":"chr1:1-100","contig":"chr1","start":0,"end":100,"detail_level":"Bases","vertical_scroll_points":0.0,"read_panel_states":[]}],"tracks":[],"status":[],"truncated":false}"#,
        )
        .unwrap()
    }

    #[test]
    fn same_seed_and_state_produce_the_same_expanded_scenario() {
        let specification = StressGeneratorSpecification {
            seed: 42,
            actions: 12,
            profile: StressGeneratorProfile::Aggressive,
            allowed_actions: vec![StressActionKind::CanvasPan, StressActionKind::Zoom],
            maximum_no_wait_actions: 3,
        };
        let one =
            generate_scenario("generated", &workspace(), [1280, 800], &specification).unwrap();
        let two =
            generate_scenario("generated", &workspace(), [1280, 800], &specification).unwrap();
        assert_eq!(one.scenario, two.scenario);
        assert_eq!(one.action_coverage, two.action_coverage);
    }

    #[test]
    fn mutation_preserves_scenario_validity() {
        let specification = StressGeneratorSpecification {
            seed: 1,
            actions: 2,
            profile: StressGeneratorProfile::Smoke,
            allowed_actions: vec![StressActionKind::CanvasPan],
            maximum_no_wait_actions: 1,
        };
        let source = generate_scenario("generated", &workspace(), [1280, 800], &specification)
            .unwrap()
            .scenario;
        mutate_scenario(&source, 99, 5).unwrap().validate().unwrap();
    }
}
