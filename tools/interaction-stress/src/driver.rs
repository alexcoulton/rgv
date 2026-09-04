use std::{collections::BTreeMap, path::Path};

use anyhow::{Context as _, bail};
use rgv_core::{
    AutomationRectSnapshot, MAXIMUM_STRESS_NATIVE_EVENTS, StressModifiers, StressNativeEvent,
    StressScrollUnit, StressTiming, StressTouchPhase,
};
use serde::{Deserialize, Serialize};

pub const DRIVER_SCHEMA_NAME: &str = "rgv.macos_input_driver";
pub const DRIVER_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_DRIVER_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverRequest {
    pub schema: String,
    pub schema_version: u32,
    pub request_id: String,
    pub operation: DriverOperation,
    pub application_instance_id: String,
    pub run_id: String,
    pub step_id: String,
    pub process_id: u32,
    pub expected_process_name: String,
    pub expected_window_title_prefix: String,
    pub expected_content_size: [f64; 2],
    #[serde(default)]
    pub expected_window_bounds: Option<[f64; 4]>,
    pub maximum_duration_millis: u64,
    #[serde(default)]
    pub events: Vec<CompiledDriverEvent>,
    #[serde(default)]
    pub pointer: Option<[f64; 2]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverOperation {
    Preflight,
    InspectWindow,
    Deliver,
    EmergencyRelease,
    RestorePointer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledDriverEvent {
    pub sequence: u64,
    pub at_micros: u64,
    pub kind: DriverEventKind,
    #[serde(default)]
    pub point: Option<[f64; 2]>,
    #[serde(default)]
    pub delta_x: f64,
    #[serde(default)]
    pub delta_y: f64,
    #[serde(default)]
    pub scroll_unit: StressScrollUnit,
    #[serde(default)]
    pub phase: StressTouchPhase,
    #[serde(default)]
    pub momentum_phase: StressTouchPhase,
    #[serde(default)]
    pub continuous: bool,
    #[serde(default)]
    pub key_code: Option<u16>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub focused: Option<bool>,
    #[serde(default)]
    pub logical_size: Option<[u32; 2]>,
    #[serde(default)]
    pub modifiers: StressModifiers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverEventKind {
    Move,
    PrimaryDown,
    PrimaryDrag,
    PrimaryUp,
    SecondaryDown,
    SecondaryDrag,
    SecondaryUp,
    Scroll,
    KeyDown,
    KeyUp,
    Focus,
    ResizeWindow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverResponse {
    pub schema: String,
    pub schema_version: u32,
    pub request_id: String,
    pub succeeded: bool,
    pub permission: DriverPermission,
    #[serde(default)]
    pub failure: Option<String>,
    #[serde(default)]
    pub window: Option<DriverWindow>,
    #[serde(default)]
    pub acknowledgements: Vec<DriverAcknowledgement>,
    #[serde(default)]
    pub emergency_releases: u64,
    #[serde(default)]
    pub pointer_position: Option<[f64; 2]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverPermission {
    Granted,
    Denied,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverWindow {
    pub process_id: u32,
    pub process_name: String,
    pub title: String,
    pub global_bounds: [f64; 4],
    pub content_origin: [f64; 2],
    pub content_size: [f64; 2],
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverAcknowledgement {
    pub sequence: u64,
    pub requested_at_micros: u64,
    pub attempted_at_micros: u64,
    pub succeeded: bool,
    #[serde(default)]
    pub failure: Option<String>,
}

impl DriverRequest {
    pub fn preflight(
        process_id: u32,
        application_instance_id: String,
        expected_process_name: String,
        expected_content_size: [f64; 2],
    ) -> Self {
        Self {
            schema: DRIVER_SCHEMA_NAME.to_owned(),
            schema_version: DRIVER_SCHEMA_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            operation: DriverOperation::Preflight,
            application_instance_id,
            run_id: "preflight".to_owned(),
            step_id: "preflight".to_owned(),
            process_id,
            expected_process_name,
            expected_window_title_prefix: "RGV".to_owned(),
            expected_content_size,
            expected_window_bounds: None,
            maximum_duration_millis: 5_000,
            events: Vec::new(),
            pointer: None,
        }
    }

    /// Builds a bounded, independently executable release request from an interrupted delivery.
    /// Mouse buttons and standard modifiers are released by the native helper; explicit key-down
    /// events are mirrored here so non-modifier keys cannot remain logically held either.
    pub fn emergency_release(interrupted: &Self) -> Self {
        let mut key_codes = interrupted
            .events
            .iter()
            .filter_map(|event| {
                (event.kind == DriverEventKind::KeyDown)
                    .then_some(event.key_code)
                    .flatten()
            })
            .collect::<Vec<_>>();
        key_codes.sort_unstable();
        key_codes.dedup();
        let events = key_codes
            .into_iter()
            .enumerate()
            .map(|(index, key_code)| CompiledDriverEvent {
                sequence: u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
                at_micros: 0,
                kind: DriverEventKind::KeyUp,
                point: None,
                delta_x: 0.0,
                delta_y: 0.0,
                scroll_unit: StressScrollUnit::Pixel,
                phase: StressTouchPhase::None,
                momentum_phase: StressTouchPhase::None,
                continuous: false,
                key_code: Some(key_code),
                text: None,
                focused: None,
                logical_size: None,
                modifiers: StressModifiers::default(),
            })
            .collect();
        Self {
            schema: DRIVER_SCHEMA_NAME.to_owned(),
            schema_version: DRIVER_SCHEMA_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            operation: DriverOperation::EmergencyRelease,
            application_instance_id: interrupted.application_instance_id.clone(),
            run_id: interrupted.run_id.clone(),
            step_id: interrupted.step_id.clone(),
            process_id: interrupted.process_id,
            expected_process_name: interrupted.expected_process_name.clone(),
            expected_window_title_prefix: interrupted.expected_window_title_prefix.clone(),
            expected_content_size: interrupted.expected_content_size,
            expected_window_bounds: interrupted.expected_window_bounds,
            maximum_duration_millis: 5_000,
            events,
            pointer: None,
        }
    }

    pub fn restore_pointer(
        process_id: u32,
        application_instance_id: String,
        expected_process_name: String,
        expected_content_size: [f64; 2],
        expected_window_bounds: Option<[f64; 4]>,
        pointer: [f64; 2],
    ) -> Self {
        Self {
            schema: DRIVER_SCHEMA_NAME.to_owned(),
            schema_version: DRIVER_SCHEMA_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            operation: DriverOperation::RestorePointer,
            application_instance_id,
            run_id: "pointer-restore".to_owned(),
            step_id: "pointer-restore".to_owned(),
            process_id,
            expected_process_name,
            expected_window_title_prefix: "RGV".to_owned(),
            expected_content_size,
            expected_window_bounds,
            maximum_duration_millis: 5_000,
            events: Vec::new(),
            pointer: Some(pointer),
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn compile_gesture(
        process_id: u32,
        application_instance_id: String,
        run_id: String,
        step_id: String,
        expected_process_name: String,
        expected_content_size: [f64; 2],
        expected_window_bounds: Option<[f64; 4]>,
        target: AutomationRectSnapshot,
        events: &[StressNativeEvent],
        timing: StressTiming,
        maximum_duration_millis: u64,
    ) -> anyhow::Result<Self> {
        if events.is_empty() || events.len() > MAXIMUM_STRESS_NATIVE_EVENTS {
            bail!("driver request must contain a bounded non-empty event sequence");
        }
        let mut compiled = Vec::with_capacity(events.len());
        let mut last_at = 0_u64;
        let mut last_point = Some([
            f64::from((target.left + target.right) * 0.5),
            f64::from((target.top + target.bottom) * 0.5),
        ]);
        let mut jitter = match timing {
            StressTiming::SeededJitter { seed, .. } => seed.max(1),
            _ => 1,
        };
        for (index, event) in events.iter().enumerate() {
            let at_micros = scheduled_micros(event.at_micros(), timing, &mut jitter, last_at);
            last_at = at_micros;
            let mut output = CompiledDriverEvent {
                sequence: u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
                at_micros,
                kind: DriverEventKind::Move,
                point: None,
                delta_x: 0.0,
                delta_y: 0.0,
                scroll_unit: StressScrollUnit::Pixel,
                phase: StressTouchPhase::None,
                momentum_phase: StressTouchPhase::None,
                continuous: false,
                key_code: None,
                text: None,
                focused: None,
                logical_size: None,
                modifiers: StressModifiers::default(),
            };
            match event {
                StressNativeEvent::Move {
                    position,
                    modifiers,
                    ..
                } => {
                    output.kind = DriverEventKind::Move;
                    output.point = Some(resolve_point(target, *position));
                    output.modifiers = *modifiers;
                    last_point = output.point;
                }
                StressNativeEvent::PrimaryDown { modifiers, .. } => {
                    output.kind = DriverEventKind::PrimaryDown;
                    output.point = last_point;
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::PrimaryDrag {
                    position,
                    modifiers,
                    ..
                } => {
                    output.kind = DriverEventKind::PrimaryDrag;
                    output.point = Some(resolve_point(target, *position));
                    output.modifiers = *modifiers;
                    last_point = output.point;
                }
                StressNativeEvent::PrimaryUp { modifiers, .. } => {
                    output.kind = DriverEventKind::PrimaryUp;
                    output.point = last_point;
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::SecondaryDown { modifiers, .. } => {
                    output.kind = DriverEventKind::SecondaryDown;
                    output.point = last_point;
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::SecondaryDrag {
                    position,
                    modifiers,
                    ..
                } => {
                    output.kind = DriverEventKind::SecondaryDrag;
                    output.point = Some(resolve_point(target, *position));
                    output.modifiers = *modifiers;
                    last_point = output.point;
                }
                StressNativeEvent::SecondaryUp { modifiers, .. } => {
                    output.kind = DriverEventKind::SecondaryUp;
                    output.point = last_point;
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::Scroll {
                    delta_x,
                    delta_y,
                    unit,
                    phase,
                    momentum_phase,
                    continuous,
                    modifiers,
                    ..
                } => {
                    output.kind = DriverEventKind::Scroll;
                    output.point = last_point;
                    output.delta_x = *delta_x;
                    output.delta_y = *delta_y;
                    output.scroll_unit = *unit;
                    output.phase = *phase;
                    output.momentum_phase = *momentum_phase;
                    output.continuous = *continuous;
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::KeyDown {
                    key_code,
                    text,
                    modifiers,
                    ..
                } => {
                    output.kind = DriverEventKind::KeyDown;
                    output.key_code = Some(*key_code);
                    output.text.clone_from(text);
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::KeyUp {
                    key_code,
                    modifiers,
                    ..
                } => {
                    output.kind = DriverEventKind::KeyUp;
                    output.key_code = Some(*key_code);
                    output.modifiers = *modifiers;
                }
                StressNativeEvent::Focus { focused, .. } => {
                    output.kind = DriverEventKind::Focus;
                    output.focused = Some(*focused);
                }
                StressNativeEvent::ResizeWindow { logical_size, .. } => {
                    output.kind = DriverEventKind::ResizeWindow;
                    output.logical_size = Some(*logical_size);
                }
                StressNativeEvent::Unsupported { class, reason, .. } => {
                    bail!("unsupported recorded event {class} cannot be replayed: {reason}");
                }
            }
            compiled.push(output);
        }
        validate_compiled_lifecycle(&compiled)?;
        let schedule_millis = last_at.div_ceil(1_000);
        if schedule_millis > maximum_duration_millis {
            bail!("compiled event schedule exceeds the gesture duration limit");
        }
        let request = Self {
            schema: DRIVER_SCHEMA_NAME.to_owned(),
            schema_version: DRIVER_SCHEMA_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            operation: DriverOperation::Deliver,
            application_instance_id,
            run_id,
            step_id,
            process_id,
            expected_process_name,
            expected_window_title_prefix: "RGV".to_owned(),
            expected_content_size,
            expected_window_bounds,
            maximum_duration_millis,
            events: compiled,
            pointer: None,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema != DRIVER_SCHEMA_NAME || self.schema_version != DRIVER_SCHEMA_VERSION {
            bail!("unsupported macOS driver protocol");
        }
        if self.process_id == 0
            || self.application_instance_id.is_empty()
            || self.run_id.is_empty()
            || self.step_id.is_empty()
            || self.expected_process_name.is_empty()
            || self.request_id.is_empty()
            || self.maximum_duration_millis == 0
        {
            bail!("driver request identity and limits must be non-empty");
        }
        if self.operation == DriverOperation::Deliver && self.events.is_empty() {
            bail!("deliver requests require events");
        }
        if matches!(
            self.operation,
            DriverOperation::Preflight
                | DriverOperation::InspectWindow
                | DriverOperation::RestorePointer
        ) && !self.events.is_empty()
        {
            bail!("inspection requests cannot contain events");
        }
        if self.operation == DriverOperation::EmergencyRelease
            && self
                .events
                .iter()
                .any(|event| event.kind != DriverEventKind::KeyUp || event.key_code.is_none())
        {
            bail!("emergency release requests may contain only explicit key releases");
        }
        if self.operation == DriverOperation::RestorePointer {
            if self
                .pointer
                .is_none_or(|point| point.into_iter().any(|value| !value.is_finite()))
            {
                bail!("restore_pointer requires a finite point");
            }
        } else if self.pointer.is_some() {
            bail!("only restore_pointer requests may contain a pointer");
        }
        if self.events.len() > MAXIMUM_STRESS_NATIVE_EVENTS {
            bail!("driver request exceeds the event bound");
        }
        if self.expected_window_bounds.is_some_and(|bounds| {
            bounds.into_iter().any(|value| !value.is_finite())
                || bounds[2] <= 0.0
                || bounds[3] <= 0.0
        }) {
            bail!("expected window bounds must be finite and non-empty");
        }
        let mut previous = 0;
        for (index, event) in self.events.iter().enumerate() {
            if index > 0 && event.at_micros < previous {
                bail!("compiled driver timestamps must be monotonic");
            }
            previous = event.at_micros;
            if event
                .point
                .is_some_and(|[x, y]| !x.is_finite() || !y.is_finite())
                || !event.delta_x.is_finite()
                || !event.delta_y.is_finite()
            {
                bail!("driver coordinates and deltas must be finite");
            }
        }
        Ok(())
    }
}

fn validate_compiled_lifecycle(events: &[CompiledDriverEvent]) -> anyhow::Result<()> {
    let mut primary_down = false;
    let mut secondary_down = false;
    let mut scroll_active = false;
    let mut keys_down = BTreeMap::<u16, u64>::new();
    for event in events {
        match event.kind {
            DriverEventKind::PrimaryDown => {
                if primary_down {
                    bail!("gesture contains a second primary press before release");
                }
                primary_down = true;
            }
            DriverEventKind::PrimaryDrag if !primary_down => {
                bail!("primary drag requires a preceding primary press");
            }
            DriverEventKind::PrimaryUp => {
                if !primary_down {
                    bail!("primary release requires a preceding primary press");
                }
                primary_down = false;
            }
            DriverEventKind::SecondaryDown => {
                if secondary_down {
                    bail!("gesture contains a second secondary press before release");
                }
                secondary_down = true;
            }
            DriverEventKind::SecondaryDrag if !secondary_down => {
                bail!("secondary drag requires a preceding secondary press");
            }
            DriverEventKind::SecondaryUp => {
                if !secondary_down {
                    bail!("secondary release requires a preceding secondary press");
                }
                secondary_down = false;
            }
            DriverEventKind::Scroll => match event.phase {
                StressTouchPhase::Began => {
                    if scroll_active {
                        bail!("scroll began before the preceding scroll lifecycle ended");
                    }
                    scroll_active = true;
                }
                StressTouchPhase::Changed => scroll_active = true,
                StressTouchPhase::Ended | StressTouchPhase::Cancelled => {
                    if !scroll_active {
                        bail!("scroll termination requires a preceding began/changed event");
                    }
                    scroll_active = false;
                }
                StressTouchPhase::None | StressTouchPhase::MayBegin => {}
            },
            DriverEventKind::KeyDown => {
                let key_code = event
                    .key_code
                    .context("key-down event omitted its key code")?;
                if keys_down.insert(key_code, event.at_micros).is_some() {
                    bail!("gesture repeats key-down {key_code} before key-up");
                }
            }
            DriverEventKind::KeyUp => {
                let key_code = event
                    .key_code
                    .context("key-up event omitted its key code")?;
                if keys_down.remove(&key_code).is_none() {
                    bail!("key-up {key_code} requires a preceding key-down");
                }
            }
            DriverEventKind::Focus if event.focused == Some(false) => {
                primary_down = false;
                secondary_down = false;
                scroll_active = false;
                keys_down.clear();
            }
            DriverEventKind::Move
            | DriverEventKind::Focus
            | DriverEventKind::ResizeWindow
            | DriverEventKind::PrimaryDrag
            | DriverEventKind::SecondaryDrag => {}
        }
    }
    if primary_down || secondary_down || scroll_active || !keys_down.is_empty() {
        bail!("gesture leaves pointer, scroll, or keyboard state held");
    }
    Ok(())
}

impl DriverResponse {
    pub fn read(path: impl AsRef<Path>, request_id: &str) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if std::fs::metadata(path)
            .with_context(|| format!("could not inspect driver response {}", path.display()))?
            .len()
            > MAXIMUM_DRIVER_RESPONSE_BYTES
        {
            bail!("macOS driver response exceeded the bounded response limit");
        }
        let response: Self = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("could not read driver response {}", path.display()))?,
        )
        .context("could not parse the macOS driver response")?;
        if response.schema != DRIVER_SCHEMA_NAME
            || response.schema_version != DRIVER_SCHEMA_VERSION
            || response.request_id != request_id
        {
            bail!("driver response did not match the request protocol and identity");
        }
        Ok(response)
    }
}

fn resolve_point(rect: AutomationRectSnapshot, point: rgv_core::StressPoint) -> [f64; 2] {
    [
        f64::from(rect.left) + point.x * f64::from(rect.right - rect.left),
        f64::from(rect.top) + point.y * f64::from(rect.bottom - rect.top),
    ]
}

fn scheduled_micros(
    recorded: u64,
    timing: StressTiming,
    jitter_state: &mut u64,
    previous: u64,
) -> u64 {
    let base = match timing {
        StressTiming::Recorded | StressTiming::SeededJitter { .. } => recorded,
        StressTiming::Scaled { factor } => (recorded as f64 * factor).round() as u64,
        StressTiming::MaximumRate => previous,
    };
    let StressTiming::SeededJitter {
        maximum_jitter_micros,
        ..
    } = timing
    else {
        return base.max(previous);
    };
    *jitter_state ^= *jitter_state << 13;
    *jitter_state ^= *jitter_state >> 7;
    *jitter_state ^= *jitter_state << 17;
    let span = maximum_jitter_micros.saturating_mul(2).saturating_add(1);
    let offset = if span == 0 { 0 } else { *jitter_state % span };
    base.saturating_add(offset)
        .saturating_sub(maximum_jitter_micros)
        .max(previous)
}

#[cfg(test)]
mod tests {
    use rgv_core::{StressPoint, StressTiming};

    use super::*;

    #[test]
    fn semantic_points_compile_to_window_local_coordinates() {
        let request = DriverRequest::compile_gesture(
            4,
            "instance-1".to_owned(),
            "run-1".to_owned(),
            "gesture-1".to_owned(),
            "rgv-desktop".to_owned(),
            [1280.0, 800.0],
            None,
            AutomationRectSnapshot {
                left: 100.0,
                top: 50.0,
                right: 500.0,
                bottom: 250.0,
            },
            &[
                StressNativeEvent::Move {
                    at_micros: 0,
                    position: StressPoint { x: 0.25, y: 0.5 },
                    modifiers: StressModifiers::default(),
                },
                StressNativeEvent::PrimaryDown {
                    at_micros: 100,
                    modifiers: StressModifiers::default(),
                },
                StressNativeEvent::PrimaryUp {
                    at_micros: 200,
                    modifiers: StressModifiers::default(),
                },
            ],
            StressTiming::Recorded,
            1_000,
        )
        .unwrap();
        assert_eq!(request.events[0].point, Some([200.0, 150.0]));
        assert_eq!(request.events[1].point, Some([200.0, 150.0]));
    }

    #[test]
    fn seeded_jitter_is_deterministic_and_monotonic() {
        let timing = StressTiming::SeededJitter {
            seed: 7,
            maximum_jitter_micros: 30,
        };
        let mut left = 7;
        let mut right = 7;
        let one = (0..10)
            .scan(0, |previous, value| {
                *previous = scheduled_micros(value * 100, timing, &mut left, *previous);
                Some(*previous)
            })
            .collect::<Vec<_>>();
        let two = (0..10)
            .scan(0, |previous, value| {
                *previous = scheduled_micros(value * 100, timing, &mut right, *previous);
                Some(*previous)
            })
            .collect::<Vec<_>>();
        assert_eq!(one, two);
        assert!(one.windows(2).all(|window| window[0] <= window[1]));
    }

    #[test]
    fn unsafe_held_gestures_are_rejected_and_have_an_emergency_key_release() {
        let error = DriverRequest::compile_gesture(
            4,
            "instance-1".to_owned(),
            "run-1".to_owned(),
            "gesture-1".to_owned(),
            "rgv-desktop".to_owned(),
            [1280.0, 800.0],
            None,
            AutomationRectSnapshot {
                left: 0.0,
                top: 0.0,
                right: 100.0,
                bottom: 100.0,
            },
            &[StressNativeEvent::KeyDown {
                at_micros: 0,
                key_code: 12,
                text: None,
                modifiers: StressModifiers::default(),
            }],
            StressTiming::Recorded,
            1_000,
        )
        .unwrap_err();
        assert!(error.to_string().contains("held"));

        let mut interrupted = DriverRequest::preflight(
            4,
            "instance-1".to_owned(),
            "rgv-desktop".to_owned(),
            [1280.0, 800.0],
        );
        interrupted.operation = DriverOperation::Deliver;
        interrupted.events.push(CompiledDriverEvent {
            sequence: 1,
            at_micros: 0,
            kind: DriverEventKind::KeyDown,
            point: None,
            delta_x: 0.0,
            delta_y: 0.0,
            scroll_unit: StressScrollUnit::Pixel,
            phase: StressTouchPhase::None,
            momentum_phase: StressTouchPhase::None,
            continuous: false,
            key_code: Some(12),
            text: None,
            focused: None,
            logical_size: None,
            modifiers: StressModifiers::default(),
        });
        let release = DriverRequest::emergency_release(&interrupted);
        assert_eq!(release.operation, DriverOperation::EmergencyRelease);
        assert_eq!(release.events.len(), 1);
        assert_eq!(release.events[0].kind, DriverEventKind::KeyUp);
        assert!(release.validate().is_ok());
    }
}
