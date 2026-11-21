//! The `Diagnostics` facade without the `diagnostics` feature: the same surface as `enabled.rs`
//! and the `Viewer` hooks of the recorder modules, with every operation a no-op.
//!
//! Nothing is recorded. Commands that would start a recording report that diagnostics are not
//! compiled in, and the stress-health endpoint serves an empty snapshot for this process.

use super::{HealthSignal, LatencyEvent, ManualDiagnosticFramePhase, ManualDiagnosticTraceFields};
use crate::{Viewer, ui};
use rgv_core::{
    AutomationInteractionTrace, AutomationWorkspaceSnapshot, InteractionRecordingStatus,
    NativeObservationSummary, PanelId, StressHealthSnapshot, StressTarget, TrackId,
};
use std::path::PathBuf;
use std::time::Duration;
use winit::event::{DeviceEvent, WindowEvent};

const UNAVAILABLE: &str = "this build of RGV does not include diagnostics";

#[derive(Clone)]
pub(crate) struct HealthHandle {
    application_instance_id: String,
}

impl HealthHandle {
    pub(crate) fn new(application_instance_id: String) -> Self {
        Self {
            application_instance_id,
        }
    }

    #[allow(clippy::unused_self, dead_code)]
    pub(crate) fn signal(&self, _signal: HealthSignal) {}

    pub(crate) fn snapshot(&self) -> StressHealthSnapshot {
        StressHealthSnapshot {
            application_instance_id: self.application_instance_id.clone(),
            process_id: std::process::id(),
            ..StressHealthSnapshot::default()
        }
    }
}

/// Uninhabited: no native-input observation is ever pending without diagnostics.
pub(crate) enum PendingNativeInputObservation {}

pub(crate) struct Diagnostics;

#[allow(clippy::unused_self, clippy::needless_pass_by_value)]
impl Diagnostics {
    pub(crate) fn new(
        _health: HealthHandle,
        _trace_output_directory: PathBuf,
        _gpu_adapter: String,
        _gpu_backend: String,
    ) -> Self {
        Self
    }

    pub(crate) fn health(&self, _signal: HealthSignal) {}

    pub(crate) fn latency_active(&self) -> bool {
        false
    }

    pub(crate) fn latency(&mut self, _event: LatencyEvent) {}

    pub(crate) fn latency_snapshot(&self) -> Option<AutomationInteractionTrace> {
        None
    }

    pub(crate) fn recording(&self) -> bool {
        false
    }

    pub(crate) fn event(&mut self, _category: &str, _name: &str, _fields: serde_json::Value) {}

    pub(crate) fn frame_phase(&mut self, _phase: ManualDiagnosticFramePhase, _duration: Duration) {}

    pub(crate) fn display_marked_dirty(
        &mut self,
        _reason: &'static str,
        _caller: &std::panic::Location<'_>,
        _was_already_dirty: bool,
    ) {
    }

    pub(crate) fn display_cleared_dirty(
        &mut self,
        _reason: &'static str,
        _caller: &std::panic::Location<'_>,
        _was_dirty: bool,
    ) {
    }

    pub(crate) fn begin_native_observation(
        &mut self,
        _run_id: String,
        _step_id: String,
        _expected_target: Option<StressTarget>,
    ) -> Result<NativeObservationSummary, String> {
        Err(UNAVAILABLE.to_owned())
    }

    pub(crate) fn end_native_observation(&mut self) -> Result<NativeObservationSummary, String> {
        Err(UNAVAILABLE.to_owned())
    }

    pub(crate) fn checkpoint_recording(
        &mut self,
        _label: String,
        _workspace: AutomationWorkspaceSnapshot,
    ) -> Result<InteractionRecordingStatus, String> {
        Err(UNAVAILABLE.to_owned())
    }
}

#[allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    clippy::needless_pass_by_ref_mut,
    clippy::too_many_arguments
)]
impl Viewer {
    pub(crate) fn maybe_start_pending_manual_diagnostic_trace(&mut self) {
        self.manual_diagnostic_trace_autostart_pending = false;
    }

    pub(crate) fn start_manual_diagnostic_trace(&mut self, _trigger: &str, _show: bool) {
        tracing::warn!("{UNAVAILABLE}; the diagnostic trace was not started");
    }

    pub(crate) fn stop_and_save_manual_diagnostic_trace(&mut self) {}

    pub(crate) fn save_recording_manual_diagnostic_trace_before_exit(&mut self) {}

    pub(crate) fn manual_diagnostic_trace_snapshot(&self) -> ui::ManualInteractionTraceSnapshot {
        ui::ManualInteractionTraceSnapshot::default()
    }

    pub(crate) fn record_manual_window_event(&mut self, _event: &WindowEvent, _consumed: bool) {}

    pub(crate) fn record_manual_device_event(&mut self, _event: &DeviceEvent) {}

    pub(crate) fn begin_manual_diagnostic_frame(&mut self) {}

    pub(crate) fn finish_manual_diagnostic_frame(&mut self, _total: Duration) {}

    pub(crate) fn record_manual_navigation_intent(&mut self, _kind: &str) {}

    pub(crate) fn record_manual_vertical_scroll_intent(
        &mut self,
        _track_id: TrackId,
        _requested: f32,
        _previous: f32,
        _maximum: f32,
        _applied: f32,
        _source: &str,
    ) {
    }

    pub(crate) fn freeze_manual_diagnostic_trace(
        &mut self,
        _category: &str,
        _name: &str,
        _message: &str,
        _fields: ManualDiagnosticTraceFields,
    ) {
    }

    pub(crate) fn queue_manual_diagnostic_trace_failure(
        &mut self,
        _category: &str,
        _name: &str,
        _message: &str,
        _fields: ManualDiagnosticTraceFields,
    ) {
    }

    pub(crate) fn alignment_filter_diagnostic_state(
        &self,
        _panel_id: PanelId,
        _track_id: TrackId,
    ) -> serde_json::Value {
        serde_json::Value::Null
    }

    pub(crate) fn manual_read_panel_scroll_cache_diagnostic_state(
        &self,
        _panel_id: PanelId,
        _track_id: TrackId,
    ) -> serde_json::Value {
        serde_json::Value::Null
    }

    pub(crate) fn manual_active_scroll_cache_diagnostic_state(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    pub(crate) fn start_interaction_recording(
        &mut self,
        _path: PathBuf,
        _fixture: Option<String>,
        _record_text: bool,
    ) -> Result<InteractionRecordingStatus, String> {
        Err(UNAVAILABLE.to_owned())
    }

    pub(crate) fn stop_interaction_recording(
        &mut self,
    ) -> Result<InteractionRecordingStatus, String> {
        Err(UNAVAILABLE.to_owned())
    }

    pub(crate) fn abort_interaction_recording_before_exit(&mut self) {}

    pub(crate) fn interaction_recording_status(&self) -> InteractionRecordingStatus {
        InteractionRecordingStatus::default()
    }

    pub(crate) fn prepare_native_input_observation(
        &mut self,
        _event: &WindowEvent,
        _egui_consumed: bool,
    ) -> Option<PendingNativeInputObservation> {
        None
    }

    pub(crate) fn mark_native_input_handled(_pending: &mut Option<PendingNativeInputObservation>) {}

    pub(crate) fn finish_native_input_observation(
        &mut self,
        _pending: Option<PendingNativeInputObservation>,
    ) {
    }
}
