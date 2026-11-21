use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use rgv_core::{PanelId, PresentationNodeId, TrackChannel, TrackId};

use crate::{
    AutomationBatch, AutomationCommand, AutomationError, AutomationRectSnapshot,
    AutomationWorkspaceSnapshot,
};

pub const INTERACTION_STRESS_SCHEMA_NAME: &str = "rgv.interaction_stress";
pub const INTERACTION_STRESS_SCHEMA_VERSION: u32 = 1;
pub const INTERACTION_RECORDING_SCHEMA_NAME: &str = "rgv.interaction_recording";
pub const INTERACTION_RECORDING_SCHEMA_VERSION: u32 = 1;
pub const STRESS_HEALTH_SCHEMA_VERSION: u32 = 1;
pub const MAXIMUM_STRESS_STEPS: usize = 10_000;
pub const MAXIMUM_STRESS_NATIVE_EVENTS: usize = 100_000;
pub const MAXIMUM_STRESS_STRING_BYTES: usize = 16_384;
pub const MAXIMUM_STRESS_REPEAT_COUNT: usize = 10_000;
pub const MAXIMUM_STRESS_NESTING: usize = 4;
pub const MAXIMUM_STRESS_RUN_MILLIS: u64 = 24 * 60 * 60 * 1_000;
pub const MAXIMUM_STRESS_COORDINATE_FRACTION: f64 = 4.0;
pub const MAXIMUM_STRESS_TIMING_SCALE: f64 = 100.0;
pub const MAXIMUM_INTERACTION_STRESS_DOCUMENT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionStressScenario {
    pub schema: String,
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub requirements: StressRequirements,
    pub steps: Vec<InteractionStressStep>,
    #[serde(default)]
    pub limits: StressRunLimits,
}

impl InteractionStressScenario {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, InteractionStressError> {
        let path = path.as_ref();
        if std::fs::metadata(path)?.len() > MAXIMUM_INTERACTION_STRESS_DOCUMENT_BYTES {
            return Err(InteractionStressError::Invalid(format!(
                "scenario exceeds the {MAXIMUM_INTERACTION_STRESS_DOCUMENT_BYTES}-byte document limit"
            )));
        }
        let mut scenario = Self::from_json(&std::fs::read_to_string(path)?)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        scenario.resolve_paths(std::path::absolute(parent)?);
        scenario.validate()?;
        Ok(scenario)
    }

    pub fn from_json(json: &str) -> Result<Self, InteractionStressError> {
        let scenario: Self = serde_json::from_str(json)?;
        scenario.validate()?;
        Ok(scenario)
    }

    pub fn to_json(&self) -> Result<String, InteractionStressError> {
        self.validate()?;
        let mut redacted = self.clone();
        redact_control_steps(&mut redacted.steps)?;
        Ok(format!("{}\n", serde_json::to_string_pretty(&redacted)?))
    }

    pub fn resolve_paths(&mut self, root: impl AsRef<Path>) {
        resolve_control_paths(&mut self.steps, root.as_ref());
    }

    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> Result<(), InteractionStressError> {
        if self.schema != INTERACTION_STRESS_SCHEMA_NAME {
            return Err(InteractionStressError::Invalid(format!(
                "scenario schema must be {INTERACTION_STRESS_SCHEMA_NAME}"
            )));
        }
        if self.schema_version != INTERACTION_STRESS_SCHEMA_VERSION {
            return Err(InteractionStressError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        validate_string(&self.name, "scenario name")?;
        if self.steps.is_empty() || self.steps.len() > MAXIMUM_STRESS_STEPS {
            return Err(InteractionStressError::Invalid(format!(
                "scenario must contain 1..={MAXIMUM_STRESS_STEPS} top-level steps"
            )));
        }
        self.requirements.validate()?;
        self.limits.validate()?;
        let mut counts = StressValidationCounts::default();
        validate_steps(&self.steps, 0, &mut counts)?;
        if counts.expanded_steps > MAXIMUM_STRESS_STEPS {
            return Err(InteractionStressError::Invalid(format!(
                "expanded scenario exceeds {MAXIMUM_STRESS_STEPS} steps"
            )));
        }
        if counts.native_events > self.limits.maximum_native_events
            || counts.native_events > MAXIMUM_STRESS_NATIVE_EVENTS
        {
            return Err(InteractionStressError::Invalid(format!(
                "scenario expands to {} native events, above its limit of {}",
                counts.native_events, self.limits.maximum_native_events
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StressRequirements {
    #[serde(default)]
    pub fixture: Option<String>,
    #[serde(default)]
    pub window_logical_size: Option<[u32; 2]>,
    #[serde(default)]
    pub scale_factor: Option<f64>,
    #[serde(default)]
    pub source_fingerprints: Vec<String>,
}

impl StressRequirements {
    pub(crate) fn validate(&self) -> Result<(), InteractionStressError> {
        if let Some(fixture) = &self.fixture {
            validate_string(fixture, "fixture")?;
        }
        if let Some([width, height]) = self.window_logical_size
            && (width < 320 || height < 240 || width > 16_384 || height > 16_384)
        {
            return Err(InteractionStressError::Invalid(
                "window_logical_size must be within 320x240..=16384x16384".to_owned(),
            ));
        }
        if let Some(scale) = self.scale_factor
            && (!scale.is_finite() || !(0.5..=8.0).contains(&scale))
        {
            return Err(InteractionStressError::Invalid(
                "scale_factor must be finite and within 0.5..=8.0".to_owned(),
            ));
        }
        if self.source_fingerprints.len() > 1_024 {
            return Err(InteractionStressError::Invalid(
                "at most 1024 source fingerprints are supported".to_owned(),
            ));
        }
        for fingerprint in &self.source_fingerprints {
            validate_string(fingerprint, "source fingerprint")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StressRunLimits {
    #[serde(default = "default_maximum_stress_duration_millis")]
    pub maximum_duration_millis: u64,
    #[serde(default = "default_maximum_stress_native_events")]
    pub maximum_native_events: usize,
    #[serde(default = "default_maximum_held_millis")]
    pub maximum_held_millis: u64,
}

impl Default for StressRunLimits {
    fn default() -> Self {
        Self {
            maximum_duration_millis: default_maximum_stress_duration_millis(),
            maximum_native_events: default_maximum_stress_native_events(),
            maximum_held_millis: default_maximum_held_millis(),
        }
    }
}

impl StressRunLimits {
    fn validate(self) -> Result<(), InteractionStressError> {
        if self.maximum_duration_millis == 0
            || self.maximum_duration_millis > MAXIMUM_STRESS_RUN_MILLIS
        {
            return Err(InteractionStressError::Invalid(format!(
                "maximum_duration_millis must be within 1..={MAXIMUM_STRESS_RUN_MILLIS}"
            )));
        }
        if self.maximum_native_events == 0
            || self.maximum_native_events > MAXIMUM_STRESS_NATIVE_EVENTS
        {
            return Err(InteractionStressError::Invalid(format!(
                "maximum_native_events must be within 1..={MAXIMUM_STRESS_NATIVE_EVENTS}"
            )));
        }
        if self.maximum_held_millis == 0 || self.maximum_held_millis > self.maximum_duration_millis
        {
            return Err(InteractionStressError::Invalid(
                "maximum_held_millis must be positive and no longer than the run".to_owned(),
            ));
        }
        Ok(())
    }
}

const fn default_maximum_stress_duration_millis() -> u64 {
    5 * 60 * 1_000
}

const fn default_maximum_stress_native_events() -> usize {
    100_000
}

const fn default_maximum_held_millis() -> u64 {
    30_000
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InteractionStressStep {
    Control {
        commands: Vec<AutomationCommand>,
    },
    Gesture {
        id: String,
        target: StressTarget,
        #[serde(default)]
        fragile_target: bool,
        #[serde(default)]
        timing: StressTiming,
        events: Vec<StressNativeEvent>,
    },
    Barrier {
        predicate: StressPredicate,
        #[serde(default = "default_stress_barrier_timeout_millis")]
        timeout_millis: u64,
    },
    Assert {
        predicate: StressPredicate,
    },
    Checkpoint {
        label: String,
        #[serde(default)]
        capture_native_window: bool,
    },
    Generated {
        specification: StressGeneratorSpecification,
    },
    Repeat {
        count: usize,
        steps: Vec<InteractionStressStep>,
    },
}

const fn default_stress_barrier_timeout_millis() -> u64 {
    30_000
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StressTarget {
    /// The whole workspace rectangle of the window.
    Window,
    /// The genomic canvas of one panel.
    GenomicCanvas {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
    },
    /// The reads-channel content of one alignment track in one panel.
    ReadCanvas {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
        /// Positive stable ID of the alignment track whose read rows are targeted.
        track_id: TrackId,
    },
    /// The vertical read scrollbar of one alignment track in one panel.
    ReadScrollbar {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
        /// Positive stable ID of the alignment track whose scrollbar is targeted.
        track_id: TrackId,
    },
    /// The header of one presentation node in one panel.
    TrackHeader {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
        /// The presentation node whose header is targeted.
        target: PresentationNodeId,
    },
    /// The overview row of one panel's header.
    Overview {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
    },
    /// The ruler row of one panel's header.
    Ruler {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
    },
    /// The content rectangle of one presentation node in one panel.
    PaneContent {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
        /// The presentation node whose content is targeted.
        target: PresentationNodeId,
    },
    /// The header rectangle of one presentation node in one panel's pane layout.
    PaneHeader {
        /// Positive stable ID of the panel that contains the target.
        panel_id: PanelId,
        /// The presentation node whose header is targeted.
        target: PresentationNodeId,
    },
    /// A user-interface element addressed by its accessibility identifier.
    Accessibility {
        /// Accessibility identifier of the element. The set of identifiers is unverified.
        identifier: String,
    },
    /// Raw window coordinates, valid only at one window size.
    WindowCoordinate {
        /// Window client-area size `[width, height]` in logical points that the coordinates
        /// were recorded at.
        required_logical_size: [u32; 2],
    },
}

impl StressTarget {
    pub fn resolve_rect(
        &self,
        workspace: &AutomationWorkspaceSnapshot,
    ) -> Option<AutomationRectSnapshot> {
        let layout = workspace.workspace_layout.as_ref()?;
        match self {
            Self::Window => Some(layout.workspace),
            Self::GenomicCanvas { panel_id } => layout
                .panels
                .iter()
                .find(|panel| panel.panel_id == *panel_id)
                .map(|panel| panel.canvas),
            Self::ReadCanvas { panel_id, track_id } => workspace
                .pane_layouts
                .iter()
                .find(|pane| pane.panel_id == *panel_id && pane.channel == TrackChannel::Reads)
                .and_then(|pane| {
                    pane.items.iter().find_map(|item| {
                        matches!(
                            item.id,
                            PresentationNodeId::Track {
                                channel: TrackChannel::Reads,
                                track_id: candidate,
                            } if candidate == *track_id
                        )
                        .then_some(item.content.unwrap_or(item.row))
                    })
                }),
            Self::ReadScrollbar { panel_id, track_id } => {
                let panel_scrollbar = layout
                    .panels
                    .iter()
                    .find(|panel| panel.panel_id == *panel_id)?
                    .scrollbar;
                let row = workspace
                    .pane_layouts
                    .iter()
                    .find(|pane| pane.panel_id == *panel_id && pane.channel == TrackChannel::Reads)?
                    .items
                    .iter()
                    .find_map(|item| {
                        matches!(
                            item.id,
                            PresentationNodeId::Track {
                                channel: TrackChannel::Reads,
                                track_id: candidate,
                            } if candidate == *track_id
                        )
                        .then_some(item.row)
                    })?;
                Some(AutomationRectSnapshot {
                    left: panel_scrollbar.left,
                    top: row.top.max(panel_scrollbar.top),
                    right: panel_scrollbar.right,
                    bottom: row.bottom.min(panel_scrollbar.bottom),
                })
            }
            Self::TrackHeader { panel_id, target } | Self::PaneHeader { panel_id, target } => {
                pane_item(workspace, *panel_id, *target).map(|item| item.header)
            }
            Self::Overview { panel_id } => layout
                .panels
                .iter()
                .find(|panel| panel.panel_id == *panel_id)
                .map(|panel| panel.overview_row),
            Self::Ruler { panel_id } => layout
                .panels
                .iter()
                .find(|panel| panel.panel_id == *panel_id)
                .map(|panel| panel.ruler_row),
            Self::PaneContent { panel_id, target } => pane_item(workspace, *panel_id, *target)
                .map(|item| item.content.unwrap_or(item.row)),
            Self::Accessibility { .. } | Self::WindowCoordinate { .. } => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), InteractionStressError> {
        match self {
            Self::GenomicCanvas { panel_id }
            | Self::Overview { panel_id }
            | Self::Ruler { panel_id }
            | Self::TrackHeader { panel_id, .. }
            | Self::PaneContent { panel_id, .. }
            | Self::PaneHeader { panel_id, .. } => validate_positive(panel_id.get(), "panel_id"),
            Self::ReadCanvas { panel_id, track_id }
            | Self::ReadScrollbar { panel_id, track_id } => {
                validate_positive(panel_id.get(), "panel_id")?;
                validate_positive(track_id.get(), "track_id")
            }
            Self::Accessibility { identifier } => validate_string(identifier, "AX identifier"),
            Self::WindowCoordinate {
                required_logical_size: [width, height],
            } if *width == 0 || *height == 0 => Err(InteractionStressError::Invalid(
                "window-coordinate targets require a non-zero logical size".to_owned(),
            )),
            Self::Window | Self::WindowCoordinate { .. } => Ok(()),
        }
    }
}

fn pane_item(
    workspace: &AutomationWorkspaceSnapshot,
    panel_id: PanelId,
    target: PresentationNodeId,
) -> Option<&crate::AutomationPaneItemLayoutSnapshot> {
    workspace
        .pane_layouts
        .iter()
        .find(|pane| pane.panel_id == panel_id && pane.channel == target.channel())?
        .items
        .iter()
        .find(|item| item.id == target)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StressPoint {
    pub x: f64,
    pub y: f64,
}

impl StressPoint {
    fn validate(self) -> Result<(), InteractionStressError> {
        if !self.x.is_finite()
            || !self.y.is_finite()
            || self.x.abs() > MAXIMUM_STRESS_COORDINATE_FRACTION
            || self.y.abs() > MAXIMUM_STRESS_COORDINATE_FRACTION
        {
            return Err(InteractionStressError::Invalid(format!(
                "target-relative coordinates must be finite and within ±{MAXIMUM_STRESS_COORDINATE_FRACTION}"
            )));
        }
        Ok(())
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StressModifiers {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub option: bool,
    #[serde(default)]
    pub command: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressScrollUnit {
    Line,
    #[default]
    Pixel,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressTouchPhase {
    #[default]
    None,
    Began,
    Changed,
    Ended,
    Cancelled,
    MayBegin,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StressNativeEvent {
    Move {
        at_micros: u64,
        position: StressPoint,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    PrimaryDown {
        at_micros: u64,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    PrimaryDrag {
        at_micros: u64,
        position: StressPoint,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    PrimaryUp {
        at_micros: u64,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    SecondaryDown {
        at_micros: u64,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    SecondaryDrag {
        at_micros: u64,
        position: StressPoint,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    SecondaryUp {
        at_micros: u64,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    Scroll {
        at_micros: u64,
        delta_x: f64,
        delta_y: f64,
        #[serde(default)]
        unit: StressScrollUnit,
        #[serde(default)]
        phase: StressTouchPhase,
        #[serde(default)]
        momentum_phase: StressTouchPhase,
        #[serde(default)]
        continuous: bool,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    KeyDown {
        at_micros: u64,
        key_code: u16,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    KeyUp {
        at_micros: u64,
        key_code: u16,
        #[serde(default)]
        modifiers: StressModifiers,
    },
    Focus {
        at_micros: u64,
        focused: bool,
    },
    ResizeWindow {
        at_micros: u64,
        logical_size: [u32; 2],
    },
    Unsupported {
        at_micros: u64,
        class: String,
        reason: String,
    },
}

impl StressNativeEvent {
    pub const fn at_micros(&self) -> u64 {
        match self {
            Self::Move { at_micros, .. }
            | Self::PrimaryDown { at_micros, .. }
            | Self::PrimaryDrag { at_micros, .. }
            | Self::PrimaryUp { at_micros, .. }
            | Self::SecondaryDown { at_micros, .. }
            | Self::SecondaryDrag { at_micros, .. }
            | Self::SecondaryUp { at_micros, .. }
            | Self::Scroll { at_micros, .. }
            | Self::KeyDown { at_micros, .. }
            | Self::KeyUp { at_micros, .. }
            | Self::Focus { at_micros, .. }
            | Self::ResizeWindow { at_micros, .. }
            | Self::Unsupported { at_micros, .. } => *at_micros,
        }
    }

    fn validate(&self) -> Result<(), InteractionStressError> {
        match self {
            Self::Move { position, .. }
            | Self::PrimaryDrag { position, .. }
            | Self::SecondaryDrag { position, .. } => position.validate(),
            Self::Scroll {
                delta_x, delta_y, ..
            } if !delta_x.is_finite()
                || !delta_y.is_finite()
                || delta_x.abs() > 1_000_000.0
                || delta_y.abs() > 1_000_000.0 =>
            {
                Err(InteractionStressError::Invalid(
                    "scroll deltas must be finite and bounded".to_owned(),
                ))
            }
            Self::KeyDown {
                text: Some(text), ..
            } => validate_string(text, "key text"),
            Self::ResizeWindow {
                logical_size: [width, height],
                ..
            } if *width == 0 || *height == 0 || *width > 16_384 || *height > 16_384 => {
                Err(InteractionStressError::Invalid(
                    "window resize must be within 1x1..=16384x16384".to_owned(),
                ))
            }
            Self::Unsupported { class, reason, .. } => {
                validate_string(class, "unsupported event class")?;
                validate_string(reason, "unsupported event reason")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum StressTiming {
    #[default]
    Recorded,
    Scaled {
        factor: f64,
    },
    MaximumRate,
    SeededJitter {
        seed: u64,
        maximum_jitter_micros: u64,
    },
}

impl StressTiming {
    fn validate(self) -> Result<(), InteractionStressError> {
        match self {
            Self::Scaled { factor }
                if !factor.is_finite()
                    || !(1.0 / MAXIMUM_STRESS_TIMING_SCALE..=MAXIMUM_STRESS_TIMING_SCALE)
                        .contains(&factor) =>
            {
                Err(InteractionStressError::Invalid(format!(
                    "timing scale must be finite and within {}..={MAXIMUM_STRESS_TIMING_SCALE}",
                    1.0 / MAXIMUM_STRESS_TIMING_SCALE
                )))
            }
            Self::SeededJitter {
                maximum_jitter_micros,
                ..
            } if maximum_jitter_micros > 60_000_000 => Err(InteractionStressError::Invalid(
                "maximum_jitter_micros must not exceed 60000000".to_owned(),
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StressPredicate {
    WorkspaceSettled,
    PanelLocus {
        panel_id: PanelId,
        locus: String,
    },
    PanelViewportChanged {
        panel_id: PanelId,
    },
    ReadScrollChanged {
        panel_id: PanelId,
        track_id: TrackId,
        #[serde(default = "default_minimum_stress_delta")]
        minimum_delta_points: f32,
    },
    GestureTerminated {
        step_id: String,
    },
    NativeRoutingComplete {
        step_id: String,
    },
    UiHeartbeatAdvanced {
        #[serde(default = "default_minimum_progress")]
        minimum_advance: u64,
    },
    PipelineProgressAdvanced {
        #[serde(default = "default_minimum_progress")]
        minimum_advance: u64,
    },
    NoCriticalFailure,
}

const fn default_minimum_progress() -> u64 {
    1
}

const fn default_minimum_stress_delta() -> f32 {
    0.5
}

impl StressPredicate {
    fn validate(&self) -> Result<(), InteractionStressError> {
        match self {
            Self::PanelLocus { panel_id, locus } => {
                validate_positive(panel_id.get(), "panel_id")?;
                validate_string(locus, "locus")
            }
            Self::PanelViewportChanged { panel_id } => {
                validate_positive(panel_id.get(), "panel_id")
            }
            Self::ReadScrollChanged {
                panel_id,
                track_id,
                minimum_delta_points,
            } => {
                validate_positive(panel_id.get(), "panel_id")?;
                validate_positive(track_id.get(), "track_id")?;
                if minimum_delta_points.is_finite() && *minimum_delta_points >= 0.0 {
                    Ok(())
                } else {
                    Err(InteractionStressError::Invalid(
                        "minimum_delta_points must be finite and non-negative".to_owned(),
                    ))
                }
            }
            Self::GestureTerminated { step_id } | Self::NativeRoutingComplete { step_id } => {
                validate_string(step_id, "step_id")
            }
            Self::UiHeartbeatAdvanced { minimum_advance }
            | Self::PipelineProgressAdvanced { minimum_advance }
                if *minimum_advance == 0 =>
            {
                Err(InteractionStressError::Invalid(
                    "minimum progress must be positive".to_owned(),
                ))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StressGeneratorSpecification {
    pub seed: u64,
    pub actions: usize,
    #[serde(default)]
    pub profile: StressGeneratorProfile,
    #[serde(default)]
    pub allowed_actions: Vec<StressActionKind>,
    #[serde(default = "default_maximum_no_wait_actions")]
    pub maximum_no_wait_actions: usize,
}

impl StressGeneratorSpecification {
    fn validate(&self) -> Result<(), InteractionStressError> {
        if self.actions == 0 || self.actions > MAXIMUM_STRESS_STEPS {
            return Err(InteractionStressError::Invalid(format!(
                "generated actions must be within 1..={MAXIMUM_STRESS_STEPS}"
            )));
        }
        if self.maximum_no_wait_actions == 0 || self.maximum_no_wait_actions > 1_000 {
            return Err(InteractionStressError::Invalid(
                "maximum_no_wait_actions must be within 1..=1000".to_owned(),
            ));
        }
        Ok(())
    }
}

const fn default_maximum_no_wait_actions() -> usize {
    8
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressGeneratorProfile {
    #[default]
    Smoke,
    RecordingMutation,
    Aggressive,
    Soak,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressActionKind {
    CanvasPan,
    CanvasPanReversal,
    ReadScrollbarDrag,
    ReadScrollbarReversal,
    VerticalScroll,
    Zoom,
    ZoomReversal,
    SecondaryClick,
    FocusCycle,
    Resize,
}

#[derive(Default)]
struct StressValidationCounts {
    expanded_steps: usize,
    native_events: usize,
    gesture_ids: BTreeSet<String>,
}

fn validate_steps(
    steps: &[InteractionStressStep],
    depth: usize,
    counts: &mut StressValidationCounts,
) -> Result<(), InteractionStressError> {
    if depth > MAXIMUM_STRESS_NESTING {
        return Err(InteractionStressError::Invalid(format!(
            "step nesting exceeds {MAXIMUM_STRESS_NESTING}"
        )));
    }
    for step in steps {
        counts.expanded_steps = counts.expanded_steps.saturating_add(1);
        match step {
            InteractionStressStep::Control { commands } => {
                AutomationBatch {
                    schema_version: crate::AUTOMATION_SCHEMA_VERSION,
                    commands: commands.clone(),
                }
                .validate()?;
            }
            InteractionStressStep::Gesture {
                id,
                target,
                fragile_target: _,
                timing,
                events,
            } => {
                validate_string(id, "gesture id")?;
                if !counts.gesture_ids.insert(id.clone()) {
                    return Err(InteractionStressError::Invalid(format!(
                        "gesture id {id:?} is defined more than once"
                    )));
                }
                target.validate()?;
                timing.validate()?;
                if events.is_empty() || events.len() > MAXIMUM_STRESS_NATIVE_EVENTS {
                    return Err(InteractionStressError::Invalid(format!(
                        "a gesture must contain 1..={MAXIMUM_STRESS_NATIVE_EVENTS} events"
                    )));
                }
                let mut previous = 0;
                for (index, event) in events.iter().enumerate() {
                    event.validate()?;
                    let at = event.at_micros();
                    if index > 0 && at < previous {
                        return Err(InteractionStressError::Invalid(
                            "gesture event timestamps must be monotonic".to_owned(),
                        ));
                    }
                    previous = at;
                }
                validate_gesture_lifecycle(events)?;
                counts.native_events = counts.native_events.saturating_add(events.len());
            }
            InteractionStressStep::Barrier {
                predicate,
                timeout_millis,
            } => {
                predicate.validate()?;
                validate_predicate_reference(predicate, &counts.gesture_ids)?;
                if *timeout_millis == 0 || *timeout_millis > MAXIMUM_STRESS_RUN_MILLIS {
                    return Err(InteractionStressError::Invalid(format!(
                        "barrier timeout must be within 1..={MAXIMUM_STRESS_RUN_MILLIS}"
                    )));
                }
            }
            InteractionStressStep::Assert { predicate } => {
                predicate.validate()?;
                validate_predicate_reference(predicate, &counts.gesture_ids)?;
            }
            InteractionStressStep::Checkpoint { label, .. } => {
                validate_string(label, "checkpoint label")?;
            }
            InteractionStressStep::Generated { specification } => {
                specification.validate()?;
                counts.expanded_steps = counts.expanded_steps.saturating_add(specification.actions);
            }
            InteractionStressStep::Repeat { count, steps } => {
                if *count == 0 || *count > MAXIMUM_STRESS_REPEAT_COUNT || steps.is_empty() {
                    return Err(InteractionStressError::Invalid(format!(
                        "repeat count must be within 1..={MAXIMUM_STRESS_REPEAT_COUNT} and contain steps"
                    )));
                }
                let before_steps = counts.expanded_steps;
                let before_events = counts.native_events;
                validate_steps(steps, depth + 1, counts)?;
                let nested_steps = counts.expanded_steps.saturating_sub(before_steps);
                let nested_events = counts.native_events.saturating_sub(before_events);
                counts.expanded_steps =
                    before_steps.saturating_add(nested_steps.saturating_mul(*count));
                counts.native_events =
                    before_events.saturating_add(nested_events.saturating_mul(*count));
            }
        }
    }
    Ok(())
}

fn validate_gesture_lifecycle(events: &[StressNativeEvent]) -> Result<(), InteractionStressError> {
    let invalid = |message: &str| InteractionStressError::Invalid(message.to_owned());
    let mut primary_down = false;
    let mut secondary_down = false;
    let mut scroll_active = false;
    let mut keys_down = BTreeSet::new();
    for event in events {
        match event {
            StressNativeEvent::PrimaryDown { .. } => {
                if primary_down {
                    return Err(invalid("gesture presses the primary button twice"));
                }
                primary_down = true;
            }
            StressNativeEvent::PrimaryDrag { .. } if !primary_down => {
                return Err(invalid("primary drag requires a preceding primary press"));
            }
            StressNativeEvent::PrimaryUp { .. } => {
                if !primary_down {
                    return Err(invalid(
                        "primary release requires a preceding primary press",
                    ));
                }
                primary_down = false;
            }
            StressNativeEvent::SecondaryDown { .. } => {
                if secondary_down {
                    return Err(invalid("gesture presses the secondary button twice"));
                }
                secondary_down = true;
            }
            StressNativeEvent::SecondaryDrag { .. } if !secondary_down => {
                return Err(invalid(
                    "secondary drag requires a preceding secondary press",
                ));
            }
            StressNativeEvent::SecondaryUp { .. } => {
                if !secondary_down {
                    return Err(invalid(
                        "secondary release requires a preceding secondary press",
                    ));
                }
                secondary_down = false;
            }
            StressNativeEvent::Scroll { phase, .. } => match phase {
                StressTouchPhase::Began => {
                    if scroll_active {
                        return Err(invalid(
                            "scroll begins before the preceding scroll lifecycle ends",
                        ));
                    }
                    scroll_active = true;
                }
                StressTouchPhase::Changed => scroll_active = true,
                StressTouchPhase::Ended | StressTouchPhase::Cancelled => {
                    if !scroll_active {
                        return Err(invalid(
                            "scroll termination requires a preceding began or changed event",
                        ));
                    }
                    scroll_active = false;
                }
                StressTouchPhase::None | StressTouchPhase::MayBegin => {}
            },
            StressNativeEvent::KeyDown { key_code, .. } => {
                if !keys_down.insert(*key_code) {
                    return Err(invalid("gesture repeats a key press before its release"));
                }
            }
            StressNativeEvent::KeyUp { key_code, .. } => {
                if !keys_down.remove(key_code) {
                    return Err(invalid(
                        "key release requires a preceding matching key press",
                    ));
                }
            }
            StressNativeEvent::Focus { focused: false, .. } => {
                primary_down = false;
                secondary_down = false;
                scroll_active = false;
                keys_down.clear();
            }
            StressNativeEvent::Move { .. }
            | StressNativeEvent::Focus { focused: true, .. }
            | StressNativeEvent::ResizeWindow { .. }
            | StressNativeEvent::Unsupported { .. }
            | StressNativeEvent::PrimaryDrag { .. }
            | StressNativeEvent::SecondaryDrag { .. } => {}
        }
    }
    if primary_down || secondary_down || scroll_active || !keys_down.is_empty() {
        return Err(invalid(
            "gesture leaves pointer, scroll, or keyboard state held",
        ));
    }
    Ok(())
}

fn validate_predicate_reference(
    predicate: &StressPredicate,
    gesture_ids: &BTreeSet<String>,
) -> Result<(), InteractionStressError> {
    let (StressPredicate::GestureTerminated { step_id }
    | StressPredicate::NativeRoutingComplete { step_id }) = predicate
    else {
        return Ok(());
    };
    if gesture_ids.contains(step_id) {
        Ok(())
    } else {
        Err(InteractionStressError::Invalid(format!(
            "predicate references gesture id {step_id:?} before it is defined"
        )))
    }
}

fn resolve_control_paths(steps: &mut [InteractionStressStep], root: &Path) {
    for step in steps {
        match step {
            InteractionStressStep::Control { commands } => {
                let mut batch = AutomationBatch {
                    schema_version: crate::AUTOMATION_SCHEMA_VERSION,
                    commands: std::mem::take(commands),
                };
                batch.resolve_paths(root);
                *commands = batch.commands;
            }
            InteractionStressStep::Repeat { steps, .. } => resolve_control_paths(steps, root),
            _ => {}
        }
    }
}

fn redact_control_steps(steps: &mut [InteractionStressStep]) -> Result<(), InteractionStressError> {
    for step in steps {
        match step {
            InteractionStressStep::Control { commands } => {
                let batch = AutomationBatch {
                    schema_version: crate::AUTOMATION_SCHEMA_VERSION,
                    commands: commands.clone(),
                };
                let redacted = AutomationBatch::from_json(&batch.to_json()?)?;
                *commands = redacted.commands;
            }
            InteractionStressStep::Repeat { steps, .. } => redact_control_steps(steps)?,
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionLifecycle {
    #[default]
    None,
    Began,
    Advanced,
    Released,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeInputObservation {
    pub sequence: u64,
    pub elapsed_micros: u64,
    #[serde(default)]
    pub step_id: Option<String>,
    pub event: StressNativeEvent,
    #[serde(default)]
    pub physical_position: Option<[f64; 2]>,
    #[serde(default)]
    pub logical_position: Option<[f64; 2]>,
    #[serde(default)]
    pub target: Option<StressTarget>,
    #[serde(default)]
    pub target_rect: Option<AutomationRectSnapshot>,
    #[serde(default)]
    pub layout_revision: u64,
    #[serde(default)]
    pub egui_consumed: bool,
    #[serde(default)]
    pub viewer_accepted: bool,
    #[serde(default)]
    pub interaction_id: Option<u64>,
    #[serde(default)]
    pub lifecycle: InteractionLifecycle,
    #[serde(default)]
    pub active_panel_id: Option<PanelId>,
    #[serde(default)]
    pub panel_interval: Option<[u64; 2]>,
    #[serde(default)]
    pub read_scroll: Option<RecordedReadScroll>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedReadScroll {
    pub panel_id: PanelId,
    pub track_id: TrackId,
    pub points: f32,
    pub maximum_points: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum InteractionRecordingEntry {
    Header(InteractionRecordingHeader),
    Input(NativeInputObservation),
    Checkpoint(InteractionRecordingCheckpoint),
    Footer(InteractionRecordingFooter),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionRecordingHeader {
    pub schema: String,
    pub schema_version: u32,
    pub recording_id: String,
    pub application_instance_id: String,
    pub process_id: u32,
    pub application_version: String,
    pub started_unix_millis: u64,
    pub surface_size_pixels: [u32; 2],
    pub scale_factor: f64,
    #[serde(default)]
    pub fixture: Option<String>,
    #[serde(default)]
    pub record_text: bool,
    #[serde(default)]
    pub initial_workspace: Option<AutomationWorkspaceSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionRecordingCheckpoint {
    pub sequence: u64,
    pub elapsed_micros: u64,
    pub label: String,
    pub workspace: AutomationWorkspaceSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionRecordingFooter {
    pub sequence: u64,
    pub elapsed_micros: u64,
    pub complete: bool,
    pub input_events: u64,
    pub coalesced_events: u64,
    pub rejected_events: u64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionRecordingState {
    #[default]
    Inactive,
    Recording,
    Complete,
    Incomplete,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InteractionRecordingStatus {
    pub state: InteractionRecordingState,
    #[serde(default)]
    pub recording_id: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub input_events: u64,
    #[serde(default)]
    pub last_sequence: u64,
    #[serde(default)]
    pub last_durable_sequence: u64,
    #[serde(default)]
    pub coalesced_events: u64,
    #[serde(default)]
    pub rejected_events: u64,
    #[serde(default)]
    pub failure: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeObservationSummary {
    pub run_id: String,
    pub step_id: String,
    pub started_sequence: u64,
    pub ended_sequence: u64,
    pub received_events: u64,
    pub applied_events: u64,
    pub target_acquired: bool,
    pub began: bool,
    pub advanced: bool,
    pub terminated: bool,
    pub extraneous_events: u64,
    #[serde(default)]
    pub event_shape: NativeEventShape,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeEventShape {
    pub move_events: u64,
    pub primary_down_events: u64,
    pub primary_drag_events: u64,
    pub primary_up_events: u64,
    pub secondary_down_events: u64,
    pub secondary_drag_events: u64,
    pub secondary_up_events: u64,
    pub scroll_none_events: u64,
    pub scroll_began_events: u64,
    pub scroll_changed_events: u64,
    pub scroll_ended_events: u64,
    pub scroll_cancelled_events: u64,
    pub scroll_may_begin_events: u64,
    pub key_down_events: u64,
    pub key_up_events: u64,
    pub focus_events: u64,
    pub resize_events: u64,
    pub direction_reversals: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressInteractionState {
    #[default]
    Idle,
    PointerHeld,
    ScrollActive,
    Cancelled,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StressHealthSnapshot {
    pub schema_version: u32,
    pub application_instance_id: String,
    pub process_id: u32,
    pub sampled_unix_millis: u64,
    pub ui_heartbeat_sequence: u64,
    pub ui_last_elapsed_millis: u64,
    pub native_input_sequence: u64,
    pub frame_sequence: u64,
    pub desired_generation: u64,
    pub committed_generation: u64,
    pub worker_progress_sequence: u64,
    pub worker_last_elapsed_millis: u64,
    #[serde(default)]
    pub active_interaction_id: Option<u64>,
    pub interaction_state: StressInteractionState,
    #[serde(default)]
    pub terminal_failure_code: Option<String>,
    pub recording: InteractionRecordingStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressRunState {
    #[default]
    Running,
    Passed,
    Failed,
    Cancelled,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StressFailureClass {
    ProcessExit,
    UiStall,
    PipelineStall,
    ProgressChurn,
    NativeEventMissing,
    NativeEventShapeMismatch,
    ExtraneousNativeInput,
    InteractionLock,
    SemanticMismatch,
    CriticalInvariant,
    TargetGeometryChanged,
    RecordingIncomplete,
    DriverFailure,
    PermissionFailure,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StressFailureSignature {
    pub class: StressFailureClass,
    #[serde(default)]
    pub step_id: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StressStepResult {
    pub index: usize,
    pub kind: String,
    pub succeeded: bool,
    pub elapsed_millis: f64,
    #[serde(default)]
    pub failure: Option<StressFailureSignature>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionStressRunManifest {
    pub schema_version: u32,
    pub run_id: String,
    pub state: StressRunState,
    pub scenario_name: String,
    #[serde(default)]
    pub scenario_schema: String,
    #[serde(default)]
    pub scenario_schema_version: u32,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub requirements: StressRequirements,
    #[serde(default)]
    pub limits: StressRunLimits,
    #[serde(default)]
    pub environment: StressRunEnvironment,
    pub application_instance_id: String,
    pub process_id: u32,
    pub started_unix_millis: u64,
    #[serde(default)]
    pub finished_unix_millis: Option<u64>,
    pub requested_events: u64,
    pub posted_events: u64,
    pub received_events: u64,
    pub applied_events: u64,
    #[serde(default)]
    pub coalesced_events: u64,
    #[serde(default)]
    pub rejected_events: u64,
    #[serde(default)]
    pub emergency_release_events: u64,
    #[serde(default)]
    pub failure: Option<StressFailureSignature>,
    pub steps: Vec<StressStepResult>,
    #[serde(default)]
    pub artifacts: Vec<StressArtifactReference>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StressRunEnvironment {
    #[serde(default)]
    pub application_version: String,
    #[serde(default)]
    pub controller_version: String,
    #[serde(default)]
    pub driver_schema_version: u32,
    #[serde(default)]
    pub automation_schema_version: u32,
    #[serde(default)]
    pub operating_system: String,
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub renderer_backend: String,
    #[serde(default)]
    pub scale_factor: f64,
    #[serde(default)]
    pub initial_surface_size_pixels: [u32; 2],
    #[serde(default)]
    pub initial_window_logical_size: [f64; 2],
    #[serde(default)]
    pub initial_window_global_bounds: Option<[f64; 4]>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StressArtifactReference {
    pub kind: String,
    pub path: PathBuf,
    #[serde(default)]
    pub sha256: Option<String>,
    pub complete: bool,
}

#[derive(Debug, Error)]
pub enum InteractionStressError {
    #[error("could not read or write interaction stress data: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not encode or decode interaction stress JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("embedded automation batch is invalid: {0}")]
    Automation(#[from] AutomationError),
    #[error("unsupported interaction stress schema version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid interaction stress document: {0}")]
    Invalid(String),
}

fn validate_positive(value: u64, label: &str) -> Result<(), InteractionStressError> {
    if value == 0 {
        Err(InteractionStressError::Invalid(format!(
            "{label} must be positive"
        )))
    } else {
        Ok(())
    }
}

fn validate_string(value: &str, label: &str) -> Result<(), InteractionStressError> {
    if value.is_empty() || value.len() > MAXIMUM_STRESS_STRING_BYTES {
        return Err(InteractionStressError::Invalid(format!(
            "{label} must contain 1..={MAXIMUM_STRESS_STRING_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(InteractionStressError::Invalid(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> AutomationWorkspaceSnapshot {
        let json = r#"{
            "application_version":"0.1.0","schema_version":6,"workspace_revision":1,
            "genome":{"name":"test","reference_configured":false,"reference_ready":false},
            "settled":true,"active_panel_id":1,
            "panels":[{"id":1,"locus":"chr1:1-10","contig":"chr1","start":0,"end":10,"detail_level":"Bases","vertical_scroll_points":0.0,"read_panel_states":[]}],
            "tracks":[],
            "pane_layouts":[{"panel_id":1,"channel":"reads","pane":{"left":10.0,"top":30.0,"right":90.0,"bottom":90.0},"content_bottom":90.0,"scroll_points":0.0,"stack_scroll_points":0.0,"items":[{"id":{"kind":"track","channel":"reads","track_id":2},"row":{"left":10.0,"top":40.0,"right":90.0,"bottom":80.0},"header":{"left":10.0,"top":40.0,"right":90.0,"bottom":50.0},"content":{"left":10.0,"top":50.0,"right":90.0,"bottom":80.0},"clip":{"left":10.0,"top":40.0,"right":90.0,"bottom":80.0},"desired_content_visible":true,"effective_content_visible":true}]}],
            "workspace_layout":{"revision":4,"workspace":{"left":0.0,"top":0.0,"right":100.0,"bottom":100.0},"properties":{"left":100.0,"top":0.0,"right":100.0,"bottom":100.0},"display_scale":1.0,"accessibility_scale":1.0,"genome_context":{"left":0.0,"top":0.0,"right":100.0,"bottom":30.0},"track_viewport":{"left":0.0,"top":30.0,"right":100.0,"bottom":100.0},"context_gutter":{"left":0.0,"top":0.0,"right":10.0,"bottom":30.0},"track_gutter":{"left":0.0,"top":30.0,"right":10.0,"bottom":100.0},"has_cytoband_row":false,"panels":[{"panel_id":1,"cytobands":"absent","column":{"left":10.0,"top":0.0,"right":100.0,"bottom":100.0},"header":{"left":10.0,"top":0.0,"right":100.0,"bottom":30.0},"header_genomic":{"left":10.0,"top":0.0,"right":90.0,"bottom":30.0},"header_scrollbar":{"left":90.0,"top":0.0,"right":100.0,"bottom":30.0},"canvas":{"left":10.0,"top":30.0,"right":90.0,"bottom":100.0},"scrollbar":{"left":90.0,"top":30.0,"right":100.0,"bottom":100.0},"metadata_row":{"left":10.0,"top":0.0,"right":90.0,"bottom":5.0},"overview_row":{"left":10.0,"top":5.0,"right":90.0,"bottom":15.0},"span_row":{"left":10.0,"top":15.0,"right":90.0,"bottom":20.0},"ruler_row":{"left":10.0,"top":20.0,"right":90.0,"bottom":30.0}}]},
            "status":[],"truncated":false
        }"#;
        serde_json::from_str(json).unwrap()
    }

    fn scenario() -> InteractionStressScenario {
        InteractionStressScenario {
            schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
            name: "round trip".to_owned(),
            seed: Some(7),
            requirements: StressRequirements::default(),
            steps: vec![InteractionStressStep::Gesture {
                id: "drag-1".to_owned(),
                target: StressTarget::ReadScrollbar {
                    panel_id: PanelId::new(1),
                    track_id: TrackId::new(2),
                },
                fragile_target: false,
                timing: StressTiming::Recorded,
                events: vec![
                    StressNativeEvent::Move {
                        at_micros: 0,
                        position: StressPoint { x: 0.5, y: 0.1 },
                        modifiers: StressModifiers::default(),
                    },
                    StressNativeEvent::PrimaryDown {
                        at_micros: 1_000,
                        modifiers: StressModifiers::default(),
                    },
                    StressNativeEvent::PrimaryDrag {
                        at_micros: 2_000,
                        position: StressPoint { x: 0.5, y: 0.9 },
                        modifiers: StressModifiers::default(),
                    },
                    StressNativeEvent::PrimaryUp {
                        at_micros: 3_000,
                        modifiers: StressModifiers::default(),
                    },
                ],
            }],
            limits: StressRunLimits::default(),
        }
    }

    #[test]
    fn scenario_round_trips_and_rejects_non_monotonic_events() {
        let valid = scenario();
        let json = valid.to_json().unwrap();
        assert_eq!(InteractionStressScenario::from_json(&json).unwrap(), valid);

        let mut invalid = valid;
        let InteractionStressStep::Gesture { events, .. } = &mut invalid.steps[0] else {
            unreachable!();
        };
        *events.last_mut().unwrap() = StressNativeEvent::PrimaryUp {
            at_micros: 500,
            modifiers: StressModifiers::default(),
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn scenario_rejects_ambiguous_ids_forward_references_and_held_input() {
        let mut duplicate = scenario();
        duplicate.steps.push(duplicate.steps[0].clone());
        assert!(
            duplicate
                .validate()
                .unwrap_err()
                .to_string()
                .contains("defined more than once")
        );

        let mut forward_reference = scenario();
        forward_reference.steps.insert(
            0,
            InteractionStressStep::Assert {
                predicate: StressPredicate::NativeRoutingComplete {
                    step_id: "drag-1".to_owned(),
                },
            },
        );
        assert!(
            forward_reference
                .validate()
                .unwrap_err()
                .to_string()
                .contains("before it is defined")
        );

        let mut held = scenario();
        if let InteractionStressStep::Gesture { events, .. } = &mut held.steps[0] {
            events.pop();
        }
        assert!(held.validate().unwrap_err().to_string().contains("held"));

        if let InteractionStressStep::Gesture { events, .. } = &mut held.steps[0] {
            events.push(StressNativeEvent::Focus {
                at_micros: 3_000,
                focused: false,
            });
        }
        held.validate().unwrap();
    }

    #[test]
    fn semantic_targets_resolve_against_existing_workspace_geometry() {
        let workspace = workspace();
        let rect = StressTarget::ReadScrollbar {
            panel_id: PanelId::new(1),
            track_id: TrackId::new(2),
        }
        .resolve_rect(&workspace)
        .unwrap();
        assert!((rect.left - 90.0).abs() < f32::EPSILON);
        assert!((rect.top - 40.0).abs() < f32::EPSILON);
        assert!((rect.bottom - 80.0).abs() < f32::EPSILON);

        let canvas = StressTarget::ReadCanvas {
            panel_id: PanelId::new(1),
            track_id: TrackId::new(2),
        }
        .resolve_rect(&workspace)
        .unwrap();
        assert!((canvas.top - 50.0).abs() < f32::EPSILON);
    }

    #[test]
    fn embedded_automation_sources_are_redacted() {
        let mut scenario = scenario();
        scenario.steps.insert(
            0,
            InteractionStressStep::Control {
                commands: vec![AutomationCommand::LoadTrack {
                    source: rgv_core::TrackSource::Remote {
                        url: "https://user:secret@example.org/reads.bam?token=x".to_owned(),
                    },
                    kind: None,
                    name: None,
                }],
            },
        );
        let json = scenario.to_json().unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("token=x"));
    }

    #[test]
    fn repeat_expansion_obeys_global_event_limit() {
        let mut scenario = scenario();
        let step = scenario.steps.remove(0);
        scenario.steps = vec![InteractionStressStep::Repeat {
            count: 9_999,
            steps: vec![step],
        }];
        assert!(scenario.validate().is_ok());
        scenario.limits.maximum_native_events = 39_995;
        assert!(scenario.validate().is_err());
    }

    #[test]
    fn health_snapshot_is_backward_compatible_with_missing_optional_fields() {
        let health: StressHealthSnapshot = serde_json::from_str(
            r#"{"schema_version":1,"application_instance_id":"a","process_id":4,"sampled_unix_millis":1,"ui_heartbeat_sequence":2,"ui_last_elapsed_millis":3,"native_input_sequence":4,"frame_sequence":5,"desired_generation":6,"committed_generation":6,"worker_progress_sequence":7,"worker_last_elapsed_millis":8,"interaction_state":"idle","recording":{"state":"inactive"}}"#,
        )
        .unwrap();
        assert_eq!(health.process_id, 4);
        assert_eq!(health.recording.state, InteractionRecordingState::Inactive);
    }
}
